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
//! `began` reads it, and how it ended, as `ended_by` does. It takes a turn you
//! began and that came to an answer, and anything else leaves the next skill to
//! wait, for you: it would rather wait for a key of the menu than run a skill
//! you did not ask for yet. A skill's own prompt is written down as yours, as
//! it goes as you would type it, and is told by its name.
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

/// The `model` Claude Code writes on an answer of its own, that no model gave.
const SYNTHETIC: &str = "<synthetic>";

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
        prompt_of(Step::FIRST.skill(), link),
    ]
}

/// What an agent is told for `skill`, of a review of `link`.
fn prompt_of(skill: &str, link: &Link) -> String {
    format!("{skill} {}", link.url())
}

/// The pull request `text` is the link of, as a review takes one: of a pull
/// request alone.
#[must_use]
pub fn pull_request(text: &str) -> Option<Link> {
    Link::parse(text).filter(|link| link.kind() == Kind::Pull)
}

/// Something for the thread of `follow` to do, which reads files and asks
/// zellij, as the core does not. Its `ticket` goes back with what came of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Read who began the last turn of the session `session`, whose review is
    /// on `skill`.
    Read { pid: u32, ticket: u64, session: SessionId, skill: &'static str },
    /// Write `prompt` in the prompt of the pane `pane` of `session`, and send
    /// it, unless you have started typing there.
    Send { pid: u32, ticket: u64, session: SessionName, pane: u32, prompt: String },
}

/// What came of an `Act`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `Act::Read`.
    Read(Began),
    /// `Act::Send`.
    Sent(Sent),
}

/// Who began the last turn of a session, as its transcript says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Began {
    /// You did, and it came to an answer.
    You,
    /// The skill it is on did: no prompt has come since its own, which goes as
    /// you would type it, and is written down as yours.
    Skill,
    /// Something else did, or you did, and interrupted it, or the API failed
    /// it.
    Other,
    /// The transcript does not say, for this reason.
    Unread(String),
}

/// What came of an `Act::Send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// It went.
    Went,
    /// It is in the prompt, and what would send it was refused, for this
    /// reason: you find it there, and your Enter sends it.
    Typed(String),
    /// Nothing went: you have started typing in the prompt.
    Draft,
    /// Nothing went: why not.
    Failed(String),
}

/// A move of a review, as a key of luneta's menu has it. One that says which
/// skill it means is refused once that is no longer next: the review may have
/// gone on since the key was shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Move {
    Hold,
    Resume,
    Next(String),
    Skip(String),
}

impl Move {
    /// The move `move_` says, of the skill `next`, as the API gives them, if
    /// they say one.
    #[must_use]
    pub fn of(move_: api::ReviewMove, next: &str) -> Option<Move> {
        let named = || (!next.is_empty()).then(|| next.to_owned());
        match move_ {
            api::ReviewMove::Hold => Some(Move::Hold),
            api::ReviewMove::Resume => Some(Move::Resume),
            api::ReviewMove::Next => named().map(Move::Next),
            api::ReviewMove::Skip => named().map(Move::Skip),
            api::ReviewMove::Unspecified => None,
        }
    }

    /// What asks for it of the agent `pid`, on the machine `on` names.
    #[must_use]
    pub fn request(self, on: Option<api::On>, pid: u32) -> api::MoveReviewRequest {
        let (does, next) = match self {
            Move::Hold => (api::ReviewMove::Hold, String::new()),
            Move::Resume => (api::ReviewMove::Resume, String::new()),
            Move::Next(next) => (api::ReviewMove::Next, next),
            Move::Skip(next) => (api::ReviewMove::Skip, next),
        };
        api::MoveReviewRequest { on, pid, does: does.into(), next }
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
    /// Another skill is next now than the one the move names.
    Moved,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::NoReview => "the agent runs no review now",
            Refused::NoNext => "the agent is on the last skill of its review",
            Refused::Going => "the next skill is on its way to the agent",
            Refused::Moved => "the review has gone on from that skill",
        })
    }
}

/// A skill of `SKILLS`. Only `FIRST` and `after` make one, so it is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step(usize);

impl Step {
    const FIRST: Step = Step(0);

    /// The skill after it, if one is.
    fn after(self) -> Option<Step> {
        let next = self.0 + 1;
        (next < SKILLS.len()).then_some(Step(next))
    }

    fn skill(self) -> &'static str {
        SKILLS[self.0]
    }

    /// Which it is, from 1, as the API counts them.
    fn number(self) -> u32 {
        u32::try_from(self.0 + 1).expect("a review has a few skills")
    }
}

/// Why the next skill waits on what is in the agent's pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blocked {
    Draft,
    Prompt,
}

/// Where a review is at, between one skill and the next one to go. `owed` says
/// that the next skill goes as this stops, with nothing more of you, as you
/// asked for it with a key, or a turn of yours ended before this one began.
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
    /// As `Stopped`, after a turn whose transcript did not say who began it.
    Unread,
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
}

impl Stage {
    /// What the next skill waits for, as the API says it.
    fn waits(self) -> api::ReviewWaits {
        match self {
            Stage::Asked | Stage::Skill { owed: false } => api::ReviewWaits::Skill,
            Stage::Stopped => api::ReviewWaits::You,
            Stage::Unread => api::ReviewWaits::Transcript,
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
        }
    }
}

/// How far a review is from its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At {
    /// No skill is left to go: it ends as the one it is on stops, once that
    /// has `begun`.
    Last { begun: bool },
    /// `next` goes after the one it is on, by `stage`, unless you hold it.
    Ahead { next: Step, stage: Stage, held: bool },
}

impl At {
    /// Where a review is as the skill `on` has been asked for.
    fn asked(on: Step, held: bool) -> Self {
        match on.after() {
            Some(next) => At::Ahead { next, stage: Stage::Asked, held },
            None => At::Last { begun: false },
        }
    }
}

/// The review an agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Review {
    link: Link,
    /// The skill it is on.
    on: Step,
    at: At,
}

/// What a review needs done to go on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Need {
    /// Read who began the turn that stopped, on the skill `skill`.
    Read { skill: &'static str },
    /// Write `prompt` in the agent's pane.
    Send { prompt: String },
}

/// What became of a `Need`, as it was handed on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Taken {
    /// It is being done, and comes back with this ticket.
    Ticket(u64),
    /// There is no room for it: it is asked for again on the next look.
    NotYet,
    /// It cannot be asked for, for this reason.
    Nowhere(String),
}

/// What a look or a key leaves of a review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum Goes {
    On,
    Over,
}

/// Says in the log that the review of `link` waits, for the reason `why`.
fn says_why(link: &Link, why: &str) {
    log::say(&format!("the review of {link} waits: {why}"));
}

/// Where a review of `link` goes that could not write the next skill, at
/// `now`, for the reason `why`, on `blocked` as it was. It goes in the log the
/// first time.
fn unwritten(link: &Link, blocked: Option<Blocked>, why: &str, now: Instant) -> Stage {
    if blocked != Some(Blocked::Prompt) {
        says_why(link, why);
    }
    Stage::Due { at: now + QUIET, blocked: Some(Blocked::Prompt) }
}

impl Review {
    /// A review of `link` whose agent came just now, on the first skill.
    fn new(link: Link) -> Self {
        Review { link, on: Step::FIRST, at: At::asked(Step::FIRST, false) }
    }

    /// Takes in what its agent is doing now, and hands `take` what it needs
    /// done.
    fn saw(
        &mut self,
        status: &Status,
        now: Instant,
        take: &mut dyn FnMut(Need) -> Taken,
    ) -> Goes {
        let busy = matches!(status, Status::Busy);
        let begins = busy || matches!(status, Status::Waiting(_));
        // A background shell lets the turn stop, and keeps the agent from
        // being idle: nothing goes until it ends, and may wake the agent.
        let stopped = matches!(status, Status::Idle | Status::Shell);
        let idle = matches!(status, Status::Idle);
        let (next, stage, held) = match &mut self.at {
            At::Last { begun: true } => return if stopped { Goes::Over } else { Goes::On },
            At::Last { begun } => {
                *begun = begins;
                return Goes::On;
            },
            At::Ahead { next, stage, held } => (*next, stage, *held),
        };
        match *stage {
            Stage::Asked if begins => *stage = Stage::Skill { owed: false },
            Stage::Skill { owed: false } if stopped => *stage = Stage::Stopped,
            Stage::Skill { owed: true } | Stage::Turn { owed: true } if stopped => {
                *stage = Stage::Due { at: now + QUIET, blocked: None };
            },
            Stage::Stopped | Stage::Unread if busy => *stage = Stage::Turn { owed: false },
            Stage::Turn { owed: false } if stopped => {
                *stage = Stage::Quiet { at: now + QUIET };
            },
            Stage::Quiet { .. } | Stage::Reading { .. } if busy => {
                *stage = Stage::Turn { owed: false };
            },
            Stage::Quiet { at } if stopped && now >= at => {
                match take(Need::Read { skill: self.on.skill() }) {
                    Taken::Ticket(ticket) => *stage = Stage::Reading { ticket },
                    Taken::NotYet => {},
                    Taken::Nowhere(why) => {
                        says_why(&self.link, &why);
                        *stage = Stage::Unread;
                    },
                }
            },
            Stage::Due { .. } if busy => *stage = Stage::Turn { owed: true },
            Stage::Due { at, blocked } if idle && !held && now >= at => {
                match take(Need::Send { prompt: prompt_of(next.skill(), &self.link) }) {
                    Taken::Ticket(ticket) => *stage = Stage::Sending { ticket, blocked },
                    Taken::NotYet => {},
                    Taken::Nowhere(why) => *stage = unwritten(&self.link, blocked, &why, now),
                }
            },
            Stage::Asked
            | Stage::Skill { .. }
            | Stage::Stopped
            | Stage::Unread
            | Stage::Turn { .. }
            | Stage::Quiet { .. }
            | Stage::Reading { .. }
            | Stage::Due { .. }
            | Stage::Sending { .. } => {},
        }
        Goes::On
    }

    /// Takes in what came of what it asked for with `ticket`, if it still
    /// waits for that.
    fn answered(&mut self, ticket: u64, answer: Answer, now: Instant) {
        let At::Ahead { next, stage, held } = &mut self.at else {
            return;
        };
        let (next, held) = (*next, *held);
        match (*stage, answer) {
            (Stage::Reading { ticket: asked }, Answer::Read(began)) if asked == ticket => {
                *stage = match began {
                    Began::You => Stage::Due { at: now, blocked: None },
                    Began::Skill | Began::Other => Stage::Stopped,
                    Began::Unread(why) => {
                        says_why(&self.link, &why);
                        Stage::Unread
                    },
                };
            },
            (Stage::Sending { ticket: asked, blocked }, Answer::Sent(sent)) if asked == ticket => {
                match sent {
                    Sent::Went => self.went(next, held),
                    Sent::Typed(why) => {
                        says_why(&self.link, &why);
                        self.went(next, held);
                    },
                    Sent::Draft => {
                        *stage = Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) };
                    },
                    Sent::Failed(why) => *stage = unwritten(&self.link, blocked, &why, now),
                }
            },
            // An answer it no longer waits for: a turn began since.
            _ => {},
        }
    }

    /// The skill `next` is the one it is on now.
    fn went(&mut self, next: Step, held: bool) {
        self.on = next;
        self.at = At::asked(next, held);
    }

    /// Does what `does` says.
    fn moved(&mut self, does: Move, now: Instant) -> Result<Goes, Refused> {
        let At::Ahead { next, stage, held } = &mut self.at else {
            return Err(Refused::NoNext);
        };
        let going = matches!(*stage, Stage::Sending { .. });
        match does {
            Move::Hold if going => return Err(Refused::Going),
            Move::Hold => *held = true,
            Move::Resume => *held = false,
            Move::Next(named) | Move::Skip(named) if named != next.skill() => {
                return Err(Refused::Moved);
            },
            Move::Next(_) => {
                *held = false;
                match stage {
                    Stage::Skill { owed } | Stage::Turn { owed } => *owed = true,
                    Stage::Asked
                    | Stage::Stopped
                    | Stage::Unread
                    | Stage::Quiet { .. }
                    | Stage::Reading { .. } => {
                        *stage = Stage::Due { at: now, blocked: None };
                    },
                    Stage::Due { at, .. } => *at = now,
                    Stage::Sending { .. } => {},
                }
            },
            Move::Skip(_) if going => return Err(Refused::Going),
            Move::Skip(_) => match (next.after(), *stage) {
                (Some(after), _) => *next = after,
                (None, Stage::Asked) => self.at = At::Last { begun: false },
                (None, Stage::Skill { .. }) => self.at = At::Last { begun: true },
                (None, _) => return Ok(Goes::Over),
            },
        }
        Ok(Goes::On)
    }

    /// The review as the API gives it.
    fn to_api(&self) -> api::Review {
        let next = match self.at {
            At::Last { .. } => None,
            At::Ahead { next, stage, held } => Some(api::review::Next {
                skill: next.skill().to_owned(),
                waits: stage.waits().into(),
                held,
            }),
        };
        api::Review {
            step: self.on.number(),
            steps: SKILLS.len() as u32,
            skill: self.on.skill().to_owned(),
            next,
        }
    }
}

/// What is kept of the review of an agent.
#[derive(Debug)]
struct Kept {
    /// When the agent's process started, as the system says it. The system can
    /// give its pid to another once it has ended.
    start: String,
    /// Where it is. None once the last skill stopped: its agent still carries
    /// its `VAR`, and takes up no other.
    review: Option<Review>,
}

/// The reviews agents run, by the agents' pids: what the core keeps of them.
#[derive(Debug)]
pub struct Reviews {
    /// When the daemon started: of an agent that started before, it does not
    /// know where the review is.
    since: SystemTime,
    by_agent: BTreeMap<u32, Kept>,
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
        self.by_agent.retain(|pid, kept| {
            agents.iter().any(|agent| agent.pid == *pid && agent.start == kept.start)
        });
        for agent in agents {
            if !self.by_agent.contains_key(&agent.pid)
                && let Some(link) = self.takes_up(agent)
            {
                let review = Some(Review::new(link));
                self.by_agent.insert(agent.pid, Kept { start: agent.start.clone(), review });
            }
            let Some(kept) = self.by_agent.get_mut(&agent.pid) else {
                continue;
            };
            let Some(review) = kept.review.as_mut() else {
                continue;
            };
            let tickets = &mut self.tickets;
            let mut take = |need| {
                let ticket = *tickets + 1;
                match asking(need, agent, sessions, ticket) {
                    Err(why) => Taken::Nowhere(why),
                    Ok(asked) => {
                        if act(asked) {
                            *tickets = ticket;
                            Taken::Ticket(ticket)
                        } else {
                            Taken::NotYet
                        }
                    },
                }
            };
            if review.saw(&agent.status, now, &mut take) == Goes::Over {
                kept.review = None;
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
        let kept = self.by_agent.get_mut(&pid);
        if let Some(review) = kept.and_then(|kept| kept.review.as_mut()) {
            review.answered(ticket, answer, now);
        }
    }

    /// Moves the review of the agent `pid`, or says why not.
    pub fn moved(&mut self, pid: u32, does: Move, now: Instant) -> Result<(), Refused> {
        let kept = self.by_agent.get_mut(&pid).ok_or(Refused::NoReview)?;
        let review = kept.review.as_mut().ok_or(Refused::NoReview)?;
        if review.moved(does, now)? == Goes::Over {
            kept.review = None;
        }
        Ok(())
    }

    /// The review of the agent `pid`, as the API gives it.
    #[must_use]
    pub fn to_api(&self, pid: u32) -> Option<api::Review> {
        self.by_agent.get(&pid)?.review.as_ref().map(Review::to_api)
    }
}

/// The act that asks for `need` of `agent`, with `ticket`, or why there is
/// none.
fn asking(need: Need, agent: &Agent, sessions: &Sessions, ticket: u64) -> Result<Act, String> {
    let pid = agent.pid;
    match need {
        Need::Read { skill } => {
            let id = agent.session_id.as_str();
            let session = SessionId::parse(id).ok_or_else(|| format!("{id} names no transcript"))?;
            Ok(Act::Read { pid, ticket, session, skill })
        },
        Need::Send { prompt } => {
            let seat = seat(agent, sessions);
            let (session, pane) =
                seat.ok_or("its session is gone, or lunetad cannot tell which it is now")?;
            Ok(Act::Send { pid, ticket, session, pane, prompt })
        },
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
                Act::Read { pid, ticket, session, skill } => {
                    let began = last_turn(projects.as_deref(), &session, skill);
                    (pid, ticket, Answer::Read(began))
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

/// Who began the last turn of the session `id`, as its transcript in `projects`
/// says, of a review on `skill`.
fn last_turn(projects: Option<&Path>, id: &SessionId, skill: &str) -> Began {
    let Some(projects) = projects else {
        return Began::Unread("lunetad finds no folder of Claude Code's".to_owned());
    };
    let Some(path) = transcripts::path_of(projects, id) else {
        return Began::Unread(format!("session {} has no transcript", id.as_str()));
    };
    last_turn_of(&path, skill).unwrap_or_else(|error| {
        Began::Unread(format!("{}: {error}", path.display()))
    })
}

/// Who began the last turn of the transcript at `path`, by the prompt that
/// began it, as `began` reads it, in the first of `WINDOWS` that has it.
fn last_turn_of(path: &Path, skill: &str) -> io::Result<Began> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    for window in WINDOWS {
        let window = window.min(length);
        file.seek(SeekFrom::Start(length - window))?;
        let mut read = Vec::new();
        file.by_ref().take(window).read_to_end(&mut read)?;
        let text = String::from_utf8_lossy(&read);
        let all = window == length;
        if let Some(began) = last_turn_in(records(&text, all).rev(), skill) {
            return Ok(began);
        }
        if all {
            break;
        }
    }
    Ok(Began::Unread(format!("{} has no prompt near its end", path.display())))
}

/// Who began the last turn of `lines`, the records of a transcript read back
/// from its end, by the prompt that began it, as `began` reads it, and by how
/// it ended, as `ended_by` does, where they tell. A turn is yours once it has
/// come to an answer: of one you interrupt before that, Claude Code writes down
/// nothing, and gives you the prompt back, while its record stays in the file.
fn last_turn_in<'a>(lines: impl Iterator<Item = &'a str>, skill: &str) -> Option<Began> {
    let mut ended = None;
    for line in lines {
        if let Some(began) = began(line, skill) {
            let answered = ended == Some(Ended::Answered);
            return Some(if began == Began::You && !answered { Began::Other } else { began });
        }
        ended = ended.or_else(|| ended_by(line));
    }
    None
}

/// The whole records of `text`, the end of a transcript, or `all` of it: what
/// follows its last line's end, Claude Code is still writing, and a window
/// starts partway into a record, and maybe a character, which reads as `�`.
fn records(text: &str, all: bool) -> std::str::Lines<'_> {
    let written = text.rsplit_once('\n').map_or("", |(written, _)| written);
    if all {
        written.lines()
    } else {
        written.split_once('\n').map_or("", |(_, whole)| whole).lines()
    }
}

/// What every record of a transcript says, that tells a prompt from the rest.
#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "isSidechain")]
    sidechain: bool,
}

/// A prompt of a transcript, as much of it as `began` reads.
#[derive(Deserialize)]
struct Record {
    #[serde(default, rename = "isMeta")]
    meta: bool,
    origin: Option<Origin>,
    #[serde(rename = "turnOrigin")]
    turn_origin: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Origin {
    kind: String,
}

#[derive(Deserialize)]
struct Message {
    model: Option<String>,
    #[serde(default)]
    content: Content,
}

/// An answer of a transcript, as much of it as `ended_by` reads.
#[derive(Deserialize)]
struct Reply {
    #[serde(default, rename = "isApiErrorMessage")]
    failed: bool,
    message: Option<Message>,
}

/// How a turn ended, by its last answer that tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    /// It said something, or used a tool.
    Answered,
    /// The API failed it, as Claude Code says.
    Failed,
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

    /// Whether an answer of this came to anything, as Claude Code tells it to
    /// give you back a prompt: it said something, or used a tool. Thinking is
    /// not yet an answer.
    fn answers(&self) -> bool {
        let Content::Blocks(blocks) = self else {
            return false;
        };
        blocks.iter().any(|block| match block.kind.as_str() {
            "tool_use" => true,
            "text" => block.text.as_deref().is_some_and(|text| !text.trim().is_empty()),
            _ => false,
        })
    }
}

/// Who began the last turn, by the record `line`, read back from the end of a
/// transcript, where that record tells: Claude Code says who wrote a prompt,
/// in its origin, and the skill `skill` goes as you would type it, so it is
/// told by its name. A notice of another session's begins a turn too, and says
/// so in `turnOrigin` alone. What says something and has no origin is no prompt
/// of yours: it writes down so that you interrupted a turn, as `[Request
/// interrupted by user]`, and a command of its own, as `/compact`. What it adds
/// around a turn, and what a tool answers within one, tell nothing, nor what a
/// subagent does. A prompt that does not read tells nothing of who began it,
/// and what came before cannot say.
fn began(line: &str, skill: &str) -> Option<Began> {
    // Most records are long, and none of those it reads.
    if !line.contains(r#""type":"user""#) {
        return None;
    }
    let unread = || Some(Began::Unread("a prompt in its transcript does not read".to_owned()));
    let Ok(head) = serde_json::from_str::<Head>(line) else {
        return unread();
    };
    if head.kind != "user" || head.sidechain {
        return None;
    }
    let Ok(record) = serde_json::from_str::<Record>(line) else {
        return unread();
    };
    let said = record.message.as_ref().and_then(|message| message.content.said());
    if let Some(origin) = record.origin {
        return Some(if origin.kind != HUMAN {
            Began::Other
        } else if said.is_some_and(|said| of_skill(said, skill)) {
            Began::Skill
        } else {
            Began::You
        });
    }
    if record.turn_origin.is_some_and(|turn| turn != HUMAN) {
        return Some(Began::Other);
    }
    if record.meta {
        return None;
    }
    said.map(|_| Began::Other)
}

/// How the turn ended, by the record `line`, read back from the end of a
/// transcript, where that record tells: an answer that says the API failed it,
/// or one that came to something. What Claude Code answers for itself says
/// nothing, as `No response requested.`, nor what a subagent does, nor an answer
/// that does not read, which can only leave the next skill waiting.
fn ended_by(line: &str) -> Option<Ended> {
    // Most records are long, and it reads no more than the last few.
    if !line.contains(r#""type":"assistant""#) {
        return None;
    }
    let head = serde_json::from_str::<Head>(line).ok()?;
    if head.kind != "assistant" || head.sidechain {
        return None;
    }
    let reply = serde_json::from_str::<Reply>(line).ok()?;
    if reply.failed {
        return Some(Ended::Failed);
    }
    let message = reply.message?;
    let given = message.model.as_deref() != Some(SYNTHETIC);
    (given && message.content.answers()).then_some(Ended::Answered)
}

/// Whether `said`, a prompt in a transcript, is the skill `skill`'s: as Claude
/// Code writes down a skill it found, or as it was typed.
fn of_skill(said: &str, skill: &str) -> bool {
    said.contains(&format!("<command-name>{skill}</command-name>"))
        || said.strip_prefix(skill).is_some_and(|args| args.starts_with(' '))
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
    if let Err(why) = action("paste", prompt) {
        return Sent::Failed(why);
    }
    match action("send-keys", "Enter") {
        Ok(()) => Sent::Went,
        Err(why) => Sent::Typed(why),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use luneta_protocol::said::Said;

    use super::*;
    use crate::agent::{Harness, Named, WaitingFor};

    const LINK: &str = "https://github.com/o/app/pull/7";

    fn link() -> Link {
        Link::parse(LINK).unwrap()
    }

    /// A review, on the skill `on`, at `stage`.
    fn review(on: usize, stage: Stage) -> Review {
        let at = At::Ahead { next: Step(on + 1), stage, held: false };
        Review { link: link(), on: Step(on), at }
    }

    /// A review on the last skill, which has `begun` or not.
    fn last(begun: bool) -> Review {
        Review { link: link(), on: Step(SKILLS.len() - 1), at: At::Last { begun } }
    }

    /// Where `review` is, which has a skill to go.
    fn stage(review: &Review) -> Stage {
        let At::Ahead { stage, .. } = review.at else {
            panic!("no skill is left to go: {review:?}");
        };
        stage
    }

    /// Whether you hold `review`, which has a skill to go.
    fn held(review: &Review) -> bool {
        let At::Ahead { held, .. } = review.at else {
            panic!("no skill is left to go: {review:?}");
        };
        held
    }

    /// What `review` needs done, once it saw `statuses`, in turn, no time
    /// apart, and only the last of them. No one takes it, so it stays there.
    fn seen(review: &mut Review, statuses: &[Status], now: Instant) -> Option<Need> {
        let mut need = None;
        for status in statuses {
            need = None;
            let goes = review.saw(status, now, &mut |it| {
                need = Some(it);
                Taken::NotYet
            });
            assert_eq!(goes, Goes::On);
        }
        need
    }

    /// Moves `review` as `does` says, which leaves it going on.
    fn moves(review: &mut Review, does: Move, now: Instant) {
        assert_eq!(review.moved(does, now), Ok(Goes::On));
    }

    /// What a review needs to read who began a turn, on the skill `on`.
    fn reads(on: usize) -> Need {
        Need::Read { skill: SKILLS[on] }
    }

    /// What a review needs to send the skill `next`.
    fn sends(next: usize) -> Need {
        Need::Send { prompt: prompt_of(SKILLS[next], &link()) }
    }

    /// A move of what it names, the skill after the first.
    fn named() -> String {
        SKILLS[1].to_owned()
    }

    /// A skill runs to its stop, and nothing goes: that stop is for you. Only
    /// a turn after it can move the review on.
    #[test]
    fn a_skill_s_stop_is_for_you_and_sends_nothing() {
        let now = Instant::now();
        let mut review = Review::new(link());
        assert_eq!(seen(&mut review, &[Status::Idle, Status::Busy], now), None);
        assert_eq!(stage(&review), Stage::Skill { owed: false });
        assert_eq!(seen(&mut review, &[Status::Idle], now), None);
        assert_eq!(stage(&review), Stage::Stopped);
        for later in [Duration::from_mins(1), Duration::from_hours(1)] {
            assert_eq!(seen(&mut review, &[Status::Idle], now + later), None);
        }
        assert_eq!(stage(&review).waits(), api::ReviewWaits::You);
    }

    /// A turn after the skill's is read, once it has stayed stopped for
    /// `QUIET`, for who began it: yours sends the next skill, and anything else
    /// waits for you again, and says so where the transcript did not tell.
    #[test]
    fn a_turn_s_stop_is_read_after_a_quiet_while() {
        let now = Instant::now();
        let quiet = now + QUIET;
        let mut stopped = review(0, Stage::Stopped);
        assert_eq!(seen(&mut stopped, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(stage(&stopped), Stage::Quiet { at: quiet });
        assert_eq!(seen(&mut stopped, &[Status::Idle], now + Duration::from_secs(4)), None);
        assert_eq!(seen(&mut stopped, &[Status::Idle], quiet), Some(reads(0)));

        let read = |began| {
            let mut reading = review(0, Stage::Reading { ticket: 1 });
            reading.answered(1, Answer::Read(began), quiet);
            reading
        };
        assert_eq!(stage(&read(Began::Skill)), Stage::Stopped);
        assert_eq!(stage(&read(Began::Other)), Stage::Stopped);
        let mut unread = read(Began::Unread("no file".to_owned()));
        assert_eq!(stage(&unread).waits(), api::ReviewWaits::Transcript);
        assert_eq!(seen(&mut unread, &[Status::Idle], quiet + QUIET), None);
        assert_eq!(seen(&mut unread, &[Status::Busy], quiet), None);
        assert_eq!(stage(&unread), Stage::Turn { owed: false });
        let mut yours = read(Began::You);
        assert_eq!(seen(&mut yours, &[Status::Idle], quiet), Some(sends(1)));
    }

    /// A turn that starts in the quiet, as a message you queued, or while the
    /// transcript is read, is the one that counts. An answer about the one
    /// before finds nothing waiting for it.
    #[test]
    fn a_turn_that_starts_in_the_quiet_is_the_one_that_counts() {
        let now = Instant::now();
        let mut quiet = review(0, Stage::Quiet { at: now + QUIET });
        assert_eq!(seen(&mut quiet, &[Status::Busy], now), None);
        assert_eq!(stage(&quiet), Stage::Turn { owed: false });
        let mut reading = review(0, Stage::Reading { ticket: 1 });
        assert_eq!(seen(&mut reading, &[Status::Busy], now), None);
        reading.answered(1, Answer::Read(Began::You), now);
        assert_eq!(stage(&reading), Stage::Turn { owed: false });
        // Of two reads, the older's answer is not the newer's.
        let mut newer = review(0, Stage::Reading { ticket: 2 });
        newer.answered(1, Answer::Read(Began::You), now);
        assert_eq!(stage(&newer), Stage::Reading { ticket: 2 });
    }

    /// Nothing goes while the agent is at work, runs a background shell, or
    /// waits on a question of its own, nor before the time it is due, nor
    /// while you hold it; and it goes on from a hold.
    #[test]
    fn a_skill_goes_only_while_the_agent_is_idle_and_not_held() {
        let now = Instant::now();
        let due = |at| Stage::Due { at, blocked: None };
        let mut idle = review(0, due(now));
        let waits = Status::Waiting(Some(WaitingFor::InputNeeded));
        for status in [&Status::Shell, &waits] {
            assert_eq!(seen(&mut idle, std::slice::from_ref(status), now), None, "{status:?}");
            assert_eq!(stage(&idle), due(now));
        }
        let mut early = review(0, due(now + QUIET));
        assert_eq!(seen(&mut early, &[Status::Idle], now), None);
        moves(&mut idle, Move::Hold, now);
        assert_eq!(seen(&mut idle, &[Status::Idle], now), None);
        moves(&mut idle, Move::Resume, now);
        assert_eq!(seen(&mut idle, &[Status::Idle], now), Some(sends(1)));
    }

    /// What wakes an agent that is due, as a message or a background task,
    /// does not lose the turn of yours that made it due.
    #[test]
    fn a_turn_that_starts_when_due_keeps_it_owed() {
        let now = Instant::now();
        let mut due = review(0, Stage::Due { at: now, blocked: None });
        assert_eq!(seen(&mut due, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(stage(&due), Stage::Due { at: now + QUIET, blocked: None });
    }

    /// A background shell lets a turn stop, which the review goes by, and the
    /// next goes once it has ended.
    #[test]
    fn a_background_shell_stops_a_turn_and_holds_the_next_skill_back() {
        let now = Instant::now();
        let mut skill = review(0, Stage::Skill { owed: false });
        assert_eq!(seen(&mut skill, &[Status::Shell], now), None);
        assert_eq!(stage(&skill), Stage::Stopped);
        let mut turn = review(0, Stage::Turn { owed: true });
        assert_eq!(seen(&mut turn, &[Status::Shell], now), None);
        assert_eq!(seen(&mut turn, &[Status::Shell], now + QUIET), None);
        assert_eq!(seen(&mut turn, &[Status::Idle], now + QUIET), Some(sends(1)));
    }

    /// The skill that went is the one it is on, the next after it is next,
    /// and it waits to begin, however long that takes: its turn is no turn of
    /// yours. A key gets the next from one that never begins.
    #[test]
    fn a_skill_that_went_is_the_one_it_is_on() {
        let now = Instant::now();
        let mut going = review(0, Stage::Sending { ticket: 3, blocked: None });
        going.answered(3, Answer::Sent(Sent::Went), now);
        assert_eq!(going, review(1, Stage::Asked));
        assert_eq!(seen(&mut going, &[Status::Idle], now + Duration::from_hours(1)), None);
        assert_eq!(stage(&going), Stage::Asked);
        let mut stuck = going.clone();
        assert_eq!(seen(&mut going, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(stage(&going), Stage::Stopped);
        moves(&mut stuck, Move::Next(SKILLS[2].to_owned()), now);
        assert_eq!(seen(&mut stuck, &[Status::Idle], now), Some(sends(2)));
    }

    /// A paste whose Enter was refused is the skill asked for, which your own
    /// Enter sends: taken for a draft of yours, that turn would be owed the
    /// same skill again.
    #[test]
    fn a_paste_with_no_enter_is_the_skill_asked_for() {
        let now = Instant::now();
        let mut going = review(0, Stage::Sending { ticket: 1, blocked: None });
        going.answered(1, Answer::Sent(Sent::Typed("no enter".to_owned())), now);
        assert_eq!(going, review(1, Stage::Asked));
    }

    /// A draft in the prompt, or a prompt it cannot see, leaves it due, and
    /// says why, until a later look finds it can write.
    #[test]
    fn what_blocks_a_skill_leaves_it_due_and_says_so() {
        let now = Instant::now();
        let mut going = review(0, Stage::Sending { ticket: 1, blocked: None });
        going.answered(1, Answer::Sent(Sent::Draft), now);
        assert_eq!(stage(&going), Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) });
        assert_eq!(stage(&going).waits(), api::ReviewWaits::Draft);
        assert_eq!(seen(&mut going, &[Status::Idle], now), None);
        assert_eq!(seen(&mut going, &[Status::Idle], now + QUIET), Some(sends(1)));
        let mut again = review(0, Stage::Sending { ticket: 2, blocked: Some(Blocked::Draft) });
        assert_eq!(stage(&again).waits(), api::ReviewWaits::Draft);
        again.answered(2, Answer::Sent(Sent::Failed("no screen".to_owned())), now);
        assert_eq!(stage(&again).waits(), api::ReviewWaits::Prompt);
        assert_eq!(again.on, Step(0));
    }

    /// `Next` asks for the next skill without a turn of yours: at once on a
    /// stop, as the turn at work stops, and from a hold. It has none to ask
    /// for on the last.
    #[test]
    fn next_needs_no_turn_of_yours() {
        let now = Instant::now();
        let mut stopped = review(0, Stage::Stopped);
        moves(&mut stopped, Move::Hold, now);
        moves(&mut stopped, Move::Next(named()), now);
        assert!(!held(&stopped));
        assert_eq!(seen(&mut stopped, &[Status::Idle], now), Some(sends(1)));

        let mut working = review(0, Stage::Skill { owed: false });
        moves(&mut working, Move::Next(named()), now);
        assert_eq!(stage(&working).waits(), api::ReviewWaits::Turn);
        assert_eq!(seen(&mut working, &[Status::Idle], now), None);
        assert_eq!(seen(&mut working, &[Status::Idle], now + QUIET), Some(sends(1)));

        let mut due = review(0, Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) });
        moves(&mut due, Move::Next(named()), now);
        assert_eq!(seen(&mut due, &[Status::Idle], now), Some(sends(1)));

        let mut unread = review(0, Stage::Unread);
        moves(&mut unread, Move::Next(named()), now);
        assert_eq!(seen(&mut unread, &[Status::Idle], now), Some(sends(1)));

        let mut on_last = last(true);
        let refused = Err(Refused::NoNext);
        assert_eq!(on_last.moved(Move::Next(named()), now), refused);
        assert_eq!(on_last.moved(Move::Skip(named()), now), refused);
        assert_eq!(on_last.moved(Move::Hold, now), refused);
    }

    /// `Skip` takes the next skill out, so that the one after goes instead.
    /// Skipping the last ends the review, or, on a skill at work, lets it end
    /// as that stops, whatever you had asked for of the next.
    #[test]
    fn skip_takes_the_next_skill_out() {
        let now = Instant::now();
        let mut stopped = review(0, Stage::Stopped);
        assert_eq!(stopped.moved(Move::Skip(named()), now), Ok(Goes::On));
        assert_eq!(stopped.to_api().next.unwrap().skill, SKILLS[2]);
        assert_eq!(stage(&stopped), Stage::Stopped);
        assert_eq!(stopped.moved(Move::Skip(SKILLS[2].to_owned()), now), Ok(Goes::Over));

        let mut working = review(1, Stage::Skill { owed: false });
        moves(&mut working, Move::Next(SKILLS[2].to_owned()), now);
        assert_eq!(working.moved(Move::Skip(SKILLS[2].to_owned()), now), Ok(Goes::On));
        assert_eq!(working.at, At::Last { begun: true });
        assert_eq!(working.to_api().next, None);
        assert_eq!(working.saw(&Status::Idle, now, &mut |_| Taken::NotYet), Goes::Over);

        let mut asked = review(1, Stage::Asked);
        assert_eq!(asked.moved(Move::Skip(SKILLS[2].to_owned()), now), Ok(Goes::On));
        assert_eq!(asked.at, At::Last { begun: false });
    }

    /// A key says which skill it means, as you were shown it. The review may
    /// have gone on since, and then it moves nothing: you would have skipped
    /// the one after, which does not come back. One of what is on its way
    /// into the pane is past stopping, so a hold of it says so.
    #[test]
    fn a_key_moves_only_the_skill_it_names() {
        let now = Instant::now();
        let shown = named();
        let mut went = review(0, Stage::Sending { ticket: 1, blocked: None });
        went.answered(1, Answer::Sent(Sent::Went), now);
        let was = went.clone();
        assert_eq!(went.moved(Move::Skip(shown.clone()), now), Err(Refused::Moved));
        assert_eq!(went.moved(Move::Next(shown.clone()), now), Err(Refused::Moved));
        assert_eq!(went, was);

        let mut going = review(0, Stage::Sending { ticket: 1, blocked: None });
        let was = going.clone();
        assert_eq!(going.moved(Move::Hold, now), Err(Refused::Going));
        assert_eq!(going.moved(Move::Skip(shown), now), Err(Refused::Going));
        assert_eq!(going, was);
    }

    /// The last skill's stop ends the review, once it has begun.
    #[test]
    fn the_last_skill_s_stop_ends_the_review() {
        let now = Instant::now();
        let mut on_last = last(false);
        assert_eq!(on_last.to_api().next, None);
        assert_eq!(on_last.saw(&Status::Idle, now, &mut |_| Taken::NotYet), Goes::On);
        assert_eq!(on_last.saw(&Status::Busy, now, &mut |_| Taken::NotYet), Goes::On);
        assert_eq!(on_last.saw(&Status::Idle, now, &mut |_| Taken::NotYet), Goes::Over);
    }

    fn agent(pid: u32, status: Status, review: Option<Link>, since: SystemTime) -> Agent {
        Agent {
            pid,
            start: "1000".to_owned(),
            harness: Harness::ClaudeCode,
            session_id: Said::new("aa-1".to_owned()).unwrap(),
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

    /// The review of the agent `pid`, which `reviews` keep, now at `stage`.
    fn put(reviews: &mut Reviews, pid: u32, stage: Stage) {
        let on = reviews.by_agent.get_mut(&pid).unwrap().review.as_mut().unwrap();
        on.at = At::Ahead { next: Step(1), stage, held: false };
    }

    /// A review starts with an agent that carries its pull request, one that
    /// started since the daemon did, at a prompt and in a pane, and goes with
    /// it. A review that ended stands while its agent does, which carries the
    /// link still. Another agent that comes to the pid has no part in it.
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

        let stranger = Agent { start: "2000".to_owned(), review: None, ..ours.clone() };
        assert_eq!(looks(&mut reviews, &[stranger], now).0, Vec::<u32>::new());
        assert_eq!(reviews.to_api(3), None);

        looks(&mut reviews, &all, now);
        reviews.by_agent.get_mut(&3).unwrap().review = None;
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
        put(&mut reviews, 3, Stage::Quiet { at: now });
        reviews.saw(std::slice::from_ref(&idle), &Sessions::default(), now, &mut |_| false);
        let waits = reviews.to_api(3).unwrap().next.unwrap().waits;
        assert_eq!(waits, i32::from(api::ReviewWaits::Turn));
        let read = |ticket| Act::Read {
            pid: 3,
            ticket,
            session: SessionId::parse("aa-1").unwrap(),
            skill: SKILLS[0],
        };
        let (_, acts) = looks(&mut reviews, std::slice::from_ref(&idle), now);
        assert_eq!(acts, [read(1)]);
        put(&mut reviews, 3, Stage::Quiet { at: now });
        let (_, acts) = looks(&mut reviews, std::slice::from_ref(&idle), now);
        assert_eq!(acts, [read(2)]);
    }

    /// One whose session cannot be told has nowhere to write, and one whose
    /// session's id names no transcript nowhere to read, and each says so.
    #[test]
    fn a_review_with_nothing_to_ask_says_so() {
        let (since, now) = (SystemTime::now(), Instant::now());
        let mut reviews = Reviews::new(since);
        let idle = agent(3, Status::Idle, Some(link()), since);
        looks(&mut reviews, std::slice::from_ref(&idle), now);
        let waits = |reviews: &Reviews| reviews.to_api(3).unwrap().next.unwrap().waits;
        put(&mut reviews, 3, Stage::Due { at: now, blocked: None });
        assert_eq!(looks(&mut reviews, std::slice::from_ref(&idle), now).1, []);
        assert_eq!(waits(&reviews), i32::from(api::ReviewWaits::Prompt));

        let odd = Agent { session_id: Said::new("../x".to_owned()).unwrap(), ..idle };
        put(&mut reviews, 3, Stage::Quiet { at: now });
        assert_eq!(looks(&mut reviews, &[odd], now).1, []);
        assert_eq!(waits(&reviews), i32::from(api::ReviewWaits::Transcript));
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
        assert_eq!(pull_request(LINK), Some(link()));
        assert_eq!(pull_request("https://github.com/o/app/issues/7"), None);
        assert_eq!(pull_request("nonsense"), None);
    }

    /// Who began a turn is said on its prompt, as real transcripts of Claude
    /// Code have them. The skill it is on is no turn of yours, though its
    /// prompt reads as one; a skill you ran of your own is. An interruption,
    /// a notice of another session's, or what has no say, is no turn of yours
    /// that finished; what a tool answered, what Claude Code adds and what a
    /// subagent did are no prompts at all. A prompt that does not read says
    /// nothing of who began it, and what is not a prompt can hold the word.
    #[test]
    fn a_transcript_says_who_began_a_turn() {
        let skill = SKILLS[1];
        let human = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"fix it"}}"#;
        let own = format!(
            r#"{{"type":"user","origin":{{"kind":"human"}},"turnOrigin":"human","message":{{"content":"<command-message>x</command-message>\n<command-name>{skill}</command-name>\n<command-args>{LINK}</command-args>"}}}}"#
        );
        let plain = format!(
            r#"{{"type":"user","origin":{{"kind":"human"}},"message":{{"content":"{skill} {LINK}"}}}}"#
        );
        let another = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"<command-name>/tigerstyle</command-name>"}}"#;
        let woke = r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"x"}}"#;
        let peer = r#"{"type":"user","isMeta":true,"origin":{"kind":"peer"},"message":{"content":"x"}}"#;
        let notice = r#"{"type":"user","isMeta":true,"turnOrigin":"system","message":{"content":"[Cross-session idle notice]"}}"#;
        let cut = r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let local = r#"{"type":"user","message":{"content":"<command-name>/compact</command-name>"}}"#;
        let shell = r#"{"type":"user","turnOrigin":"human","message":{"content":"<bash-stdout>x</bash-stdout>"}}"#;
        let answer = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}"#;
        let side = r#"{"type":"user","isSidechain":true,"origin":{"kind":"human"},"message":{"content":"x"}}"#;
        let them = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        let holds = r#"{"type":"progress","data":{"type":"user"},"message":"text"}"#;
        let shapeless = r#"{"type":"user","message":{"content":[{"kind":"new"}]}}"#;
        assert_eq!(began(human, skill), Some(Began::You));
        assert_eq!(began(another, skill), Some(Began::You));
        assert_eq!(began(&own, skill), Some(Began::Skill));
        assert_eq!(began(&plain, skill), Some(Began::Skill));
        for other in [woke, peer, notice, cut, local, shell] {
            assert_eq!(began(other, skill), Some(Began::Other), "{other}");
        }
        for none in [answer, meta, side, them, holds, "", "not json"] {
            assert_eq!(began(none, skill), None, "{none}");
        }
        for bad in [shapeless, r#"{"type":"user""#] {
            assert!(matches!(began(bad, skill), Some(Began::Unread(_))), "{bad}");
        }
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("t.jsonl");
        let reads = |lines: &[&str]| {
            let lines: Vec<String> = lines.iter().map(|line| format!("{line}\n")).collect();
            fs::write(&path, lines.concat()).unwrap();
            last_turn_of(&path, skill).unwrap()
        };
        assert_eq!(reads(&[woke, human, them, answer, them, meta]), Began::You);
        assert_eq!(reads(&[human, them, woke, them]), Began::Other);
        assert_eq!(reads(&[human, them, cut]), Began::Other);
        assert_eq!(reads(&[human, &own, meta, them, answer, them]), Began::Skill);
        assert_eq!(reads(&[&own, them, notice, them]), Began::Other);
        for none in [&[them][..], &[], &[human, them, shapeless]] {
            assert!(matches!(reads(none), Began::Unread(_)), "{none:?}");
        }
        // What Claude Code is still writing is no record yet.
        fs::write(&path, format!("{human}\n{them}\n{{\"type\":\"user\",\"mess")).unwrap();
        assert_eq!(last_turn_of(&path, skill).unwrap(), Began::You);

        // A turn of yours counts once it has said something or used a tool,
        // and its last answer is no failure: Claude Code writes down nothing
        // of a turn you interrupt before that, and takes the prompt back.
        let used = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"tool_use","id":"t","name":"Bash","input":{"type":"user"}}]}}"#;
        let thought = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm","signature":"x"}]}}"#;
        let blank = r#"{"type":"assistant","message":{"content":[{"type":"text","text":" \n"}]}}"#;
        let added = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
        let failed = r#"{"type":"assistant","isApiErrorMessage":true,"error":"server_error","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: Connection lost"}]}}"#;
        let aside = r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"x"}]}}"#;
        for finished in [&[human, used][..], &[human, them, thought], &[human, them, added]] {
            assert_eq!(reads(finished), Began::You, "{finished:?}");
        }
        for cut_short in [
            &[human][..],
            &[human, meta],
            &[human, thought],
            &[human, blank],
            &[human, added],
            &[human, aside],
            &[human, failed],
            &[human, them, failed],
            &[human, them, answer, failed, meta],
        ] {
            assert_eq!(reads(cut_short), Began::Other, "{cut_short:?}");
        }
        // The skill's own turn is told by its name, answered or not.
        assert_eq!(reads(&[&own]), Began::Skill);
        assert_eq!(reads(&[&own, failed]), Began::Skill);
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
        let them = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        fs::write(&path, format!("{human}\n{filler}\n{them}\n")).unwrap();
        assert_eq!(last_turn_of(&path, SKILLS[0]).unwrap(), Began::You);
    }

    /// A window that starts partway into a prompt does not take what is left
    /// of it for one that does not read: the next window has all of it.
    #[test]
    fn a_window_cut_through_a_prompt_reads_it_whole_in_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("t.jsonl");
        let pad = "x".repeat(500);
        let prompt = format!(
            r#"{{"pad":"{pad}","type":"user","origin":{{"kind":"human"}},"message":{{"content":"fix it"}}}}"#
        );
        // The window starts 200 bytes before the filler, within the padding.
        let shell = r#"{"type":"assistant","message":{"content":[{"type":"text","text":""}]}}"#;
        let window = usize::try_from(WINDOWS[0]).unwrap();
        let filler = "a".repeat(window - 200 - 1 - shell.len());
        let filler =
            format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{filler}"}}]}}}}"#);
        fs::write(&path, format!("{prompt}\n{filler}\n")).unwrap();
        assert_eq!(last_turn_of(&path, SKILLS[0]).unwrap(), Began::You);
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
