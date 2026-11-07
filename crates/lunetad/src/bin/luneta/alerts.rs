//! The desktop's notifications of the agents that want you, of this machine
//! and of each other that lunetad reaches: of one that waits on you, once it
//! has waited `HELD`, and of one done with its turn, at once, but while a
//! window of luneta's of this machine has the focus, where its row says so.
//! A turn is done once nothing it started runs on. Only a wait sounds. Each
//! goes once its agent no longer wants you, or once you look at its pane, as
//! you do while luneta's window has the focus and its zellij is on that pane,
//! or another luneta's of this machine; and one of a turn that ended goes as
//! a window of luneta's takes the focus. None comes of a pane you look at.
//!
//! What a notification says is in the end of its agent's transcript, which
//! is read before it goes up: the tool a permission is for, which tells a
//! plan from a command, the question asked, or what the agent said last. An
//! agent that no longer wants you by the time it is read has none.
//!
//! A machine lunetad does not reach just now keeps its agents as they were,
//! and their notifications, until it is heard again; and so does a session
//! whose panes zellij has not listed yet, as none has as lunetad starts. A
//! machine lunetad no longer watches takes its notifications with it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::agent::{Status as Reported, WaitingFor as Waits};
use lunetad::api::{Agent, Status, WaitingFor};
use lunetad::transcripts::{on_what, one_line, Reading, SessionId};
use serde_json::Value;

use crate::brief;
use crate::host::Event;
use crate::machines::Machine;
use crate::picker::{Here, Seated};
use crate::sidebar::{Listed, PanePlace};

/// How long an agent waits on you before the desktop says so: as long as
/// Claude Code waits before it tells its hooks, so that a dialog that goes
/// as it came, as one auto mode answers itself, says nothing.
pub const HELD: Duration = Duration::from_secs(6);

/// How much of the end of a transcript a notification reads: the records of
/// the last turn, which a tool's long answer can be most of.
const SHARE_BYTES: usize = 256 * 1024;

/// The longest a notification's body is, on one line: enough to carry a
/// thought, short enough that the desktop does not cut it.
const BODY_CHARS: usize = 150;

/// An agent, by the machine it runs on and its process there: no two run by
/// one pid at once, which their sessions' ids do not promise, as a session
/// resumed in another pane keeps its id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub on: Machine,
    pub pid: u32,
}

/// What an agent waits on you for, as Claude Code says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// A tool it asks to run, or a plan it asks you to take.
    Permission,
    /// A question of its own, or of a server's.
    Input,
    /// What Claude Code says in a word luneta does not know.
    Other(String),
    /// What it does not say, or says in a word only a newer daemon knows.
    Unsaid,
}

/// What an agent does, of what its notifications are of, as its daemon
/// says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    Busy,
    /// Its turn is over, and nothing it started runs on.
    Idle,
    /// It waits on you, since when Claude Code says: a wait since another
    /// moment is another wait.
    Waiting { wait: Wait, since: Option<SystemTime> },
    /// What says nothing: a turn over while a command it started in the
    /// background runs on, whose end begins another; a word luneta does not
    /// know; or one of Claude Code's own dialogs, which you opened.
    Quiet,
}

impl Doing {
    /// What `agent` does, as its daemon says.
    pub fn of(agent: &Agent) -> Doing {
        let Some(Reported::KnownStatus(known)) = &agent.status else {
            return Doing::Quiet;
        };
        match Status::try_from(*known) {
            Ok(Status::Busy) => Doing::Busy,
            Ok(Status::Idle) => Doing::Idle,
            Ok(Status::Waiting) => {
                let wait = match &agent.waiting_for {
                    Some(Waits::KnownWaitingFor(known)) => match WaitingFor::try_from(*known) {
                        Ok(WaitingFor::PermissionPrompt) => Wait::Permission,
                        Ok(WaitingFor::InputNeeded) => Wait::Input,
                        Ok(WaitingFor::DialogOpen) => return Doing::Quiet,
                        Ok(WaitingFor::Unspecified) | Err(_) => Wait::Unsaid,
                    },
                    Some(Waits::OtherWaitingFor(word)) => Wait::Other(word.clone()),
                    None => Wait::Unsaid,
                };
                let since = agent.status_since.and_then(|at| at.try_into().ok());
                Doing::Waiting { wait, since }
            },
            // Claude Code says `shell` of a turn that is over while a command
            // it started in the background runs on.
            Ok(Status::Shell | Status::Unspecified) | Err(_) => Doing::Quiet,
        }
    }
}

/// What an agent wants of you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// It waits on you.
    Wait(Wait),
    /// It is done with its turn.
    Done,
}

/// A notification to put up, once the end of its agent's transcript is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    /// Counts the notifications luneta reads for, by which one read late is
    /// told from a newer one.
    pub number: u64,
    pub key: Key,
    pub want: Want,
    /// What the sidebar names the agent's row.
    pub label: String,
    /// The id of the agent's session, which names its transcript, where it
    /// is one.
    pub transcript: Option<SessionId>,
    /// Whether it was up already, put up by another luneta of this machine,
    /// which took it down as it ended: it goes up again without a sound.
    pub again: bool,
}

/// What the desktop is to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Read the end of the agent's transcript, for the notification `Due` is.
    Read(Due),
    /// Take the agent's notification down, where one is up.
    Close(Key),
}

/// What luneta has of what runs on a machine lunetad watches.
#[derive(Debug)]
pub enum Reach {
    /// What its daemon says runs there now: the agents in the panes zellij
    /// there has listed, the sessions it has listed the panes of, and those
    /// it has not yet, whose agents cannot be placed until it has.
    Heard { agents: Vec<Seated>, listed: Vec<Listed>, unlisted: Vec<Listed> },
    /// Nothing: lunetad does not reach it just now.
    Away,
}

/// Where the notification of a turn that ended, or of a wait, is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Told {
    /// None is up, nor on its way.
    Not,
    /// The end of its transcript is being read, for the notification of
    /// this number.
    Reading(u64),
    Shown,
}

impl Told {
    /// Whether it is up, or on its way up.
    fn is_up(self) -> bool {
        match self {
            Told::Reading(_) | Told::Shown => true,
            Told::Not => false,
        }
    }
}

/// Where the notification of a wait is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    /// It waits on you since luneta heard so, at this instant, and its
    /// notification goes up once it has waited `HELD`.
    Held(Instant),
    Told(Told),
}

/// What an agent does, as last heard, and where its notification is: a turn
/// that ended has one, and so has a wait, held first. Nothing else does.
#[derive(Debug)]
enum State {
    Busy,
    Idle(Told),
    Waiting { wait: Wait, since: Option<SystemTime>, hold: Hold },
    Quiet,
}

impl State {
    /// An agent luneta begins to follow, which does as `doing` says at `now`:
    /// one that waits on you is held, but where luneta takes it as it is, as
    /// `as_is` says, which says nothing of it.
    fn new(doing: Doing, now: Instant, as_is: bool) -> State {
        match doing {
            Doing::Busy => State::Busy,
            Doing::Idle => State::Idle(Told::Not),
            Doing::Waiting { wait, since } => {
                let hold = if as_is { Hold::Told(Told::Not) } else { Hold::Held(now) };
                State::Waiting { wait, since, hold }
            },
            Doing::Quiet => State::Quiet,
        }
    }

    /// Whether the agent does as `doing` says still.
    fn is(&self, doing: &Doing) -> bool {
        match (self, doing) {
            (State::Busy, Doing::Busy)
            | (State::Idle(_), Doing::Idle)
            | (State::Quiet, Doing::Quiet) => true,
            (State::Waiting { wait, since, .. }, Doing::Waiting { wait: now, since: from }) => {
                wait == now && since == from
            },
            _ => false,
        }
    }

    /// Its notification, where it can have one: none while it works, nor
    /// while it does what says nothing, nor while its wait is held.
    fn told(&mut self) -> Option<&mut Told> {
        match self {
            State::Idle(told) | State::Waiting { hold: Hold::Told(told), .. } => Some(told),
            State::Busy | State::Waiting { hold: Hold::Held(_), .. } | State::Quiet => None,
        }
    }

    /// Whether a notification of it is up, or on its way up.
    fn is_up(&self) -> bool {
        match self {
            State::Idle(told) | State::Waiting { hold: Hold::Told(told), .. } => told.is_up(),
            State::Busy | State::Waiting { hold: Hold::Held(_), .. } | State::Quiet => false,
        }
    }

    /// Takes down what is up of it, and what is held: none goes up of it
    /// until it does something else.
    fn down(&mut self) {
        match self {
            State::Idle(told) => *told = Told::Not,
            State::Waiting { hold, .. } => *hold = Hold::Told(Told::Not),
            State::Busy | State::Quiet => {},
        }
    }
}

/// An agent luneta follows.
#[derive(Debug)]
struct Followed {
    state: State,
    place: PanePlace,
    label: String,
    /// The id of its session, as last heard, where it is one: one cleared
    /// has another.
    transcript: Option<SessionId>,
}

impl Followed {
    /// The agent of `key` does as `doing` says, as heard at `now`: what the
    /// desktop is to do of it. Whatever it does since makes what was up of it
    /// stale. A wait that begins is held; and a turn that ends is said at
    /// once, numbered after `numbered`, but where a window of luneta's has
    /// the focus, as `focused` says.
    fn does(
        &mut self,
        key: &Key,
        doing: Doing,
        now: Instant,
        numbered: &mut u64,
        focused: bool,
    ) -> Vec<Act> {
        if self.state.is(&doing) {
            return Vec::new();
        }
        let mut acts = Vec::new();
        if self.state.is_up() {
            acts.push(Act::Close(key.clone()));
        }
        let ended = matches!((&self.state, &doing), (State::Busy, Doing::Idle));
        self.state = if ended && !focused {
            *numbered += 1;
            acts.push(Act::Read(self.due(key, *numbered, Want::Done, false)));
            State::Idle(Told::Reading(*numbered))
        } else {
            State::new(doing, now, false)
        };
        acts
    }

    /// The notification of `want` numbered `number`, to read for: up
    /// `again`, or for the first time.
    fn due(&self, key: &Key, number: u64, want: Want, again: bool) -> Due {
        let (key, label, transcript) = (key.clone(), self.label.clone(), self.transcript.clone());
        Due { number, key, want, label, transcript, again }
    }
}

/// The agents of every machine luneta heard of, and their notifications.
#[derive(Debug, Default)]
pub struct Alerts {
    followed: HashMap<Key, Followed>,
    /// The sessions of each machine whose panes zellij had listed, as luneta
    /// last heard. An agent it begins to follow in one that was not among
    /// them is taken as it is: what it did before luneta could place it, and
    /// whether you were told, luneta cannot say.
    listed: HashMap<Machine, HashSet<Listed>>,
    /// The number of the last notification read for.
    numbered: u64,
    /// Whether a window of luneta's of this machine has the focus, as luneta
    /// last heard: no turn that ends is said then, as its row says it.
    focused: bool,
}

impl Alerts {
    /// The notifications of no agent yet, with a window of luneta's that has
    /// the focus where `focused` says.
    pub fn new(focused: bool) -> Alerts {
        Alerts { focused, ..Alerts::default() }
    }

    /// A window of luneta's of this machine has the focus, or none has, as
    /// `focused` says: what is up of each turn that ended goes as one takes
    /// it, and no turn that ends is said until none has it. A wait is said
    /// all the same, as it holds its agent up until you answer.
    pub fn focus(&mut self, focused: bool) -> Vec<Act> {
        let took = focused && !self.focused;
        self.focused = focused;
        let mut acts = Vec::new();
        if !took {
            return acts;
        }
        for (key, followed) in &mut self.followed {
            if let State::Idle(told) = &mut followed.state
                && told.is_up()
            {
                acts.push(Act::Close(key.clone()));
                *told = Told::Not;
            }
        }
        acts
    }

    /// Takes in what runs on each machine lunetad watches, as `machines`
    /// says, at `now`: what the desktop is to do, where an agent came to wait
    /// on you, or is done, or does anything else since, or ended. A machine
    /// lunetad does not reach just now keeps its agents as they were, and so
    /// does a session whose panes zellij has not listed yet; a machine left
    /// out of `machines`, which lunetad no longer watches, takes its agents
    /// with it.
    pub fn heard(&mut self, machines: Vec<(Machine, Reach)>, now: Instant) -> Vec<Act> {
        let mut acts = Vec::new();
        let watched: HashSet<Machine> = machines.iter().map(|(on, _)| on.clone()).collect();
        self.followed.retain(|key, followed| {
            let kept = watched.contains(&key.on);
            if !kept && followed.state.is_up() {
                acts.push(Act::Close(key.clone()));
            }
            kept
        });
        self.listed.retain(|on, _| watched.contains(on));
        for (on, reach) in machines {
            let Reach::Heard { agents, listed, unlisted } = reach else {
                continue;
            };
            let before = self.listed.insert(on.clone(), listed.into_iter().collect());
            let mut running = HashSet::with_capacity(agents.len());
            for seated in agents {
                let Seated { pid, place, label, transcript, doing, .. } = seated;
                let key = Key { on: on.clone(), pid };
                if let Some(followed) = self.followed.get_mut(&key) {
                    followed.place = place;
                    followed.label = label;
                    followed.transcript = transcript;
                    let focused = self.focused;
                    acts.extend(followed.does(&key, doing, now, &mut self.numbered, focused));
                } else {
                    let placed = before.as_ref().is_some_and(|was| was.contains(&place.session));
                    let state = State::new(doing, now, !placed);
                    self.followed.insert(key.clone(), Followed { state, place, label, transcript });
                }
                running.insert(key);
            }
            self.followed.retain(|key, followed| {
                // One in a session whose panes zellij has not listed yet may
                // run there still.
                let ended = key.on == on
                    && !running.contains(key)
                    && !unlisted.contains(&followed.place.session);
                if ended && followed.state.is_up() {
                    acts.push(Act::Close(key.clone()));
                }
                !ended
            });
        }
        acts
    }

    /// The agents in the pane `here` is on, if it is on one, by their pids.
    pub fn at(&self, here: Option<&Here>) -> Vec<Key> {
        let Some(Here::Pane { on, place }) = here else {
            return Vec::new();
        };
        let mut keys: Vec<Key> = (self.followed.iter())
            .filter(|(key, followed)| key.on == *on && followed.place == *place)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by_key(|key| key.pid);
        keys
    }

    /// You look at the panes of the agents of `keys`, in a window of
    /// luneta's that has the focus: the notification of each goes, and one
    /// held for it never goes up.
    pub fn looked(&mut self, keys: &[Key]) -> Vec<Act> {
        let mut acts = Vec::new();
        for key in keys {
            let Some(followed) = self.followed.get_mut(key) else {
                continue;
            };
            if followed.state.is_up() {
                acts.push(Act::Close(key.clone()));
            }
            followed.state.down();
        }
        acts
    }

    /// When the next agent that waits on you has waited `HELD`, if one does.
    pub fn due(&self) -> Option<Instant> {
        let held = self.followed.values().filter_map(|followed| match followed.state {
            State::Waiting { hold: Hold::Held(at), .. } => Some(at + HELD),
            State::Busy
            | State::Idle(_)
            | State::Waiting { hold: Hold::Told(_), .. }
            | State::Quiet => None,
        });
        held.min()
    }

    /// The notifications of the agents that have waited on you `HELD` by
    /// `now`, to read for.
    pub fn ticked(&mut self, now: Instant) -> Vec<Act> {
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            let State::Waiting { wait, hold, .. } = &mut followed.state else {
                continue;
            };
            let Hold::Held(at) = *hold else {
                continue;
            };
            if now < at + HELD {
                continue;
            }
            self.numbered += 1;
            *hold = Hold::Told(Told::Reading(self.numbered));
            let want = Want::Wait(wait.clone());
            acts.push(Act::Read(followed.due(key, self.numbered, want, false)));
        }
        acts
    }

    /// The waits up as far as luneta knows, to read for again, as this luneta
    /// takes over putting them up from another of this machine, which took
    /// them down as it ended: no turn that ended goes up again.
    pub fn again(&mut self) -> Vec<Act> {
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            let wait = match &mut followed.state {
                State::Idle(told) => {
                    *told = Told::Not;
                    continue;
                },
                State::Waiting { wait, hold: Hold::Told(told), .. } if *told == Told::Shown => {
                    self.numbered += 1;
                    *told = Told::Reading(self.numbered);
                    wait.clone()
                },
                State::Busy | State::Waiting { .. } | State::Quiet => continue,
            };
            acts.push(Act::Read(followed.due(key, self.numbered, Want::Wait(wait), true)));
        }
        acts
    }

    /// What was read for a notification, `read`: whether it goes up, as it
    /// does while its agent still wants what it was read for, and you have
    /// not looked.
    pub fn read(&mut self, read: &Read) -> bool {
        let Some(followed) = self.followed.get_mut(&read.alert.key) else {
            return false;
        };
        match followed.state.told() {
            Some(told) if *told == Told::Reading(read.number) => {
                *told = Told::Shown;
                true
            },
            Some(_) | None => false,
        }
    }

    /// You clicked the notification of `key`'s agent, which goes: where its
    /// pane is, as last heard, if luneta still follows it.
    pub fn clicked(&mut self, key: &Key) -> Option<PanePlace> {
        let followed = self.followed.get_mut(key)?;
        followed.state.down();
        Some(followed.place.clone())
    }
}

/// What a notification is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The agent asked you something.
    Asked,
    /// It has a plan for you to take or not.
    Plan,
    /// It asks to run a tool.
    Permission,
    /// It waits on you for something else.
    Wants,
    /// It is done with its turn.
    Done,
}

/// What a notification sounds as it goes up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Of a question, or a plan, for you to answer.
    Question,
    /// Of anything else the agent waits on you for.
    Bell,
}

/// A notification, as it goes up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub key: Key,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    /// What it sounds as it goes up, if anything: a wait does, but where it
    /// was up before, and a turn that ended never does.
    pub sound: Option<Sound>,
}

impl Alert {
    /// The notification `due` is, by the end of its agent's transcript,
    /// `end`, where it was read.
    pub fn of(due: &Due, end: Option<&str>) -> Alert {
        let turn = end.map_or_default(Turn::of);
        let tool = turn.tool.as_ref().map(|(name, input)| (name.as_str(), input));
        let (kind, body) = match (&due.want, tool) {
            (Want::Done, _) => {
                (Kind::Done, turn.said.unwrap_or_else(|| "Its turn is over".to_owned()))
            },
            (Want::Wait(Wait::Permission), Some(("ExitPlanMode", _))) => {
                (Kind::Plan, "Waiting on your review".to_owned())
            },
            (Want::Wait(Wait::Permission), tool) => {
                let body = match tool {
                    Some((name, input)) => match run_on(input) {
                        Some(what) => format!("{name}: {what}"),
                        None => name.to_owned(),
                    },
                    None => "Waiting on your permission".to_owned(),
                };
                (Kind::Permission, body)
            },
            (Want::Wait(Wait::Input), tool) => {
                let asked = tool.filter(|(name, _)| *name == "AskUserQuestion");
                let question = asked.and_then(|(_, input)| run_on(input));
                (Kind::Asked, question.unwrap_or("Waiting on your answer").to_owned())
            },
            (Want::Wait(Wait::Other(word)), _) => (Kind::Wants, word.clone()),
            (Want::Wait(Wait::Unsaid), _) => (Kind::Wants, "Waiting on you".to_owned()),
        };
        let headline = match kind {
            Kind::Asked => "asked you something",
            Kind::Plan => "has a plan for you",
            Kind::Permission => "needs permission",
            Kind::Wants => "needs you",
            Kind::Done => "done",
        };
        let title = match &due.key.on {
            Machine::Local => format!("{} — {headline}", due.label),
            Machine::Remote(destination) => format!("{} on {destination} — {headline}", due.label),
        };
        let sound = match kind {
            Kind::Asked | Kind::Plan => Some(Sound::Question),
            Kind::Permission | Kind::Wants => Some(Sound::Bell),
            Kind::Done => None,
        };
        let (key, body) = (due.key.clone(), one_line(&body, BODY_CHARS));
        Alert { key, kind, title, body, sound: sound.filter(|_| !due.again) }
    }
}

/// What the end of a transcript says of its agent's last turn.
#[derive(Debug, Default)]
struct Turn {
    /// The tool it called last that has had no answer yet, and what on: the
    /// one a permission is asked for.
    tool: Option<(String, Value)>,
    /// What it said last.
    said: Option<String>,
}

impl Turn {
    /// The turn the end of a transcript, `end`, is the end of: a JSON record
    /// to a line. A subagent's records are left out.
    fn of(end: &str) -> Turn {
        let mut calls: Vec<Call> = Vec::new();
        let mut said = None;
        for line in end.lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record["isSidechain"].as_bool() == Some(true) {
                continue;
            }
            let blocks = record["message"]["content"].as_array().into_iter().flatten();
            match record["type"].as_str() {
                Some("assistant") => {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") => said = text_of(block).or(said),
                            Some("tool_use") => calls.extend(Call::of(block)),
                            _ => {},
                        }
                    }
                },
                Some("user") => {
                    for block in blocks.filter(|block| block["type"] == "tool_result") {
                        let answered = block["tool_use_id"].as_str();
                        calls.retain(|call| Some(call.id.as_str()) != answered);
                    }
                },
                _ => {},
            }
        }
        let tool = calls.pop().map(|call| (call.name, call.input));
        Turn { tool, said }
    }
}

/// A tool the agent called, by the id its answer names.
struct Call {
    id: String,
    name: String,
    input: Value,
}

impl Call {
    fn of(block: &Value) -> Option<Call> {
        let (id, name) = (block["id"].as_str()?, block["name"].as_str()?);
        Some(Call { id: id.to_owned(), name: name.to_owned(), input: block["input"].clone() })
    }
}

/// What a tool was run on, as `on_what` says, where that is anything.
fn run_on(input: &Value) -> Option<&str> {
    on_what(input).filter(|what| !what.trim().is_empty())
}

/// What a text block of the agent's says, if anything.
fn text_of(block: &Value) -> Option<String> {
    let text = block["text"].as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What came of reading for a notification.
#[derive(Debug)]
pub struct Read {
    pub number: u64,
    pub alert: Alert,
}

/// Reads the end of the transcript of `due`'s agent on a thread of its own,
/// on this machine in `claude_folder`, the folder the daemon reads Claude
/// Code's sessions in, and on another through its daemon there, and hands
/// back the notification it makes of it. One whose end does not read, or
/// whose session names no transcript, says what `due` does alone.
pub fn read(due: Due, claude_folder: Option<PathBuf>, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let projects = claude_folder.map(|folder| folder.join("projects"));
        let end = due.transcript.clone().and_then(|id| {
            let reading = Reading::new(vec![id], SHARE_BYTES).ok()?;
            let ends = brief::read(&due.key.on, &reading, projects.as_deref()).ok()?;
            ends.into_values().next()
        });
        let alert = Alert::of(&due, end.as_deref());
        // luneta is on its way out.
        let _ = events.send(Event::Alerted(Read { number: due.number, alert }));
    });
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use serde_json::json;

    use lunetad::Destination;

    use super::*;
    use crate::sidebar::Mark;

    /// The session the tests' agents are in.
    fn work() -> Listed {
        Listed::new("work", UNIX_EPOCH + Duration::from_secs(10))
    }

    fn place(pane: u32) -> PanePlace {
        PanePlace { session: work(), pane }
    }

    /// The agent of pid `pid`, in pane `pid` of the session `work`, as the
    /// picker lists it, which does as `doing` says.
    fn seated(pid: u32, doing: Doing) -> Seated {
        Seated {
            place: place(pid),
            label: format!("agent {pid}"),
            mark: Mark::Unreported,
            status: String::new(),
            since: None,
            dir: "luneta/main".to_owned(),
            pid,
            transcript: SessionId::parse(&format!("{pid:08x}-0000-0000-0000-000000000000")),
            doing,
        }
    }

    /// A wait for `wait` since `since` seconds after the epoch.
    fn waits(wait: Wait, since: u64) -> Doing {
        Doing::Waiting { wait, since: Some(UNIX_EPOCH + Duration::from_secs(since)) }
    }

    /// What zellij has listed of the session `work`, on a machine.
    fn heard(agents: Vec<Seated>) -> Reach {
        Reach::Heard { agents, listed: vec![work()], unlisted: Vec::new() }
    }

    /// This machine's agents, and no other machine.
    fn here(agents: Vec<Seated>) -> Vec<(Machine, Reach)> {
        vec![(Machine::Local, heard(agents))]
    }

    fn nixos() -> Destination {
        "nixos".parse().expect("a destination")
    }

    fn key(pid: u32) -> Key {
        Key { on: Machine::Local, pid }
    }

    fn reads(acts: &[Act]) -> Vec<(u32, Want)> {
        let read = |act: &Act| match act {
            Act::Read(due) => Some((due.key.pid, due.want.clone())),
            Act::Close(_) => None,
        };
        acts.iter().filter_map(read).collect()
    }

    fn closes(acts: &[Act]) -> Vec<Key> {
        let closed = |act: &Act| match act {
            Act::Close(key) => Some(key.clone()),
            Act::Read(_) => None,
        };
        acts.iter().filter_map(closed).collect()
    }

    /// What was read for `due`, which says nothing of a transcript.
    fn read(due: &Due) -> Read {
        Read { number: due.number, alert: Alert::of(due, None) }
    }

    /// Puts up what `acts` read for, as luneta does once each is read.
    fn shown(alerts: &mut Alerts, acts: &[Act]) {
        for act in acts {
            if let Act::Read(due) = act {
                assert!(alerts.read(&read(due)), "{due:?} goes up");
            }
        }
    }

    /// The agents luneta hears of first are as they are: one that waited
    /// already, or finished, says nothing, as luneta cannot say whether you
    /// were told.
    #[test]
    fn what_agents_did_before_luneta_heard_of_them_says_nothing() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let first = here(vec![seated(1, waits(Wait::Permission, 5))]);
        assert_eq!(alerts.heard(first, now), []);
        assert_eq!(alerts.due(), None);
        assert_eq!(alerts.ticked(now + HELD), []);
    }

    /// An agent in a session whose panes zellij had not listed is as it is
    /// once they are, as it is in the first a daemon that starts lists; and
    /// one that runs in a session listed again since is followed still, as
    /// it is once a daemon that takes another's place has listed its panes.
    #[test]
    fn an_agent_placed_late_is_taken_as_it_is_and_one_unplaced_for_a_while_is_kept() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let unlisted = || {
            let (agents, listed, unlisted) = (Vec::new(), Vec::new(), vec![work()]);
            vec![(Machine::Local, Reach::Heard { agents, listed, unlisted })]
        };
        assert_eq!(alerts.heard(unlisted(), now), []);
        let placed = here(vec![seated(1, waits(Wait::Permission, 5)), seated(2, Doing::Busy)]);
        assert_eq!(alerts.heard(placed, now), []);
        assert_eq!(alerts.ticked(now + HELD), [], "it waited before luneta could place it");

        let acts = alerts.heard(here(vec![seated(2, Doing::Idle)]), now);
        assert_eq!(reads(&acts), [(2, Want::Done)]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.heard(unlisted(), now), [], "its daemon has not placed it yet");
        let again = here(vec![seated(2, Doing::Idle)]);
        assert_eq!(alerts.heard(again, now), [], "it is as it was");
        assert_eq!(closes(&alerts.heard(here(Vec::new()), now)), [key(2)]);
    }

    /// A wait goes up once it has lasted `HELD`, and one over before then,
    /// as a dialog auto mode answers itself, never does. A wait that ends
    /// takes its notification down.
    #[test]
    fn a_wait_goes_up_once_it_has_lasted_and_down_once_it_ends() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        alerts.heard(here(vec![seated(1, Doing::Busy)]), now);

        assert_eq!(alerts.heard(here(vec![seated(1, waits(Wait::Permission, 2))]), now), []);
        assert_eq!(alerts.due(), Some(now + HELD));
        assert_eq!(alerts.ticked(now + HELD.saturating_sub(Duration::from_millis(1))), []);
        assert_eq!(alerts.heard(here(vec![seated(1, Doing::Busy)]), now), []);
        assert_eq!(alerts.due(), None);

        let later = now + Duration::from_secs(2);
        alerts.heard(here(vec![seated(1, waits(Wait::Input, 4))]), later);
        let acts = alerts.ticked(later + HELD);
        assert_eq!(reads(&acts), [(1, Want::Wait(Wait::Input))]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.ticked(later + HELD * 2), [], "it goes up once");
        let acts = alerts.heard(here(vec![seated(1, Doing::Busy)]), later + HELD * 2);
        assert_eq!(closes(&acts), [key(1)]);
    }

    /// A turn that ends is said at once; a wait that ends in idle, as one
    /// you said no to, is not. A turn said goes once the agent works again.
    #[test]
    fn a_turn_that_ends_is_said_at_once_and_goes_once_the_agent_works_again() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let two = |one: Doing, two: Doing| here(vec![seated(1, one), seated(2, two)]);
        alerts.heard(two(Doing::Busy, waits(Wait::Permission, 1)), now);
        let acts = alerts.heard(two(Doing::Idle, Doing::Idle), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.heard(two(Doing::Idle, Doing::Idle), now), []);
        let acts = alerts.heard(two(Doing::Busy, Doing::Idle), now);
        assert_eq!(closes(&acts), [key(1)]);
    }

    /// Two agents of one session, as one resumed in a second pane, are two:
    /// each is said of as it does, and only then.
    #[test]
    fn two_agents_of_one_session_are_two() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let mut resumed = seated(2, Doing::Idle);
        resumed.transcript = seated(1, Doing::Busy).transcript;
        let both = || here(vec![seated(1, Doing::Busy), resumed.clone()]);
        alerts.heard(both(), now);
        assert_eq!(alerts.heard(both(), now), []);
        assert_eq!(alerts.heard(both(), now), [], "neither changed");
        let acts = alerts.heard(here(vec![seated(1, Doing::Idle), resumed.clone()]), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
    }

    /// What was read for an agent that no longer wants it by the time it is
    /// read does not go up, nor does what was read before a newer reading.
    #[test]
    fn what_is_read_too_late_does_not_go_up() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        alerts.heard(here(vec![seated(1, Doing::Busy)]), now);
        let acts = alerts.heard(here(vec![seated(1, Doing::Idle)]), now);
        let Act::Read(done) = &acts[0] else { unreachable!() };
        let acts = alerts.heard(here(vec![seated(1, waits(Wait::Input, 3))]), now);
        assert_eq!(closes(&acts), [key(1)], "what is on its way up is taken down too");
        assert!(!alerts.read(&read(done)));
        let acts = alerts.ticked(now + HELD);
        let Act::Read(wait) = &acts[0] else { unreachable!() };
        alerts.heard(here(vec![seated(1, Doing::Busy)]), now + HELD);
        assert!(!alerts.read(&read(wait)));
    }

    /// A dialog of Claude Code's own you opened says nothing; any other
    /// wait, of a word luneta does not know too, does.
    #[test]
    fn a_dialog_you_opened_says_nothing() {
        let waiting = |pid, waiting_for| Agent {
            pid,
            status: Some(Reported::KnownStatus(Status::Waiting.into())),
            waiting_for,
            ..Agent::default()
        };
        let known = |waits: WaitingFor| Some(Waits::KnownWaitingFor(waits.into()));
        let dialog = waiting(1, known(WaitingFor::DialogOpen));
        assert_eq!(Doing::of(&dialog), Doing::Quiet);
        let newer = waiting(2, Some(Waits::OtherWaitingFor("sandbox request".to_owned())));
        let other = Wait::Other("sandbox request".to_owned());
        assert_eq!(Doing::of(&newer), Doing::Waiting { wait: other, since: None });
        let unsaid = waiting(3, Some(Waits::KnownWaitingFor(99)));
        assert_eq!(Doing::of(&unsaid), Doing::Waiting { wait: Wait::Unsaid, since: None });

        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pid| seated(pid, Doing::Busy);
        alerts.heard(here(vec![busy(1), busy(2), busy(3)]), now);
        let all = [(1, &dialog), (2, &newer), (3, &unsaid)];
        alerts.heard(here(all.map(|(pid, agent)| seated(pid, Doing::of(agent))).into()), now);
        let mut read = reads(&alerts.ticked(now + HELD));
        read.sort_by_key(|(pid, _)| *pid);
        let other = Wait::Other("sandbox request".to_owned());
        assert_eq!(read, [(2, Want::Wait(other)), (3, Want::Wait(Wait::Unsaid))]);
    }

    /// The notification of an agent in the pane you look at goes, and one
    /// held for it never goes up: of the agents in that pane of that
    /// machine's.
    #[test]
    fn looking_at_an_agent_s_pane_takes_its_notification_down() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pid| seated(pid, Doing::Busy);
        alerts.heard(here(vec![busy(1), busy(2)]), now);
        let waiting = |pid, since| seated(pid, waits(Wait::Input, since));
        alerts.heard(here(vec![waiting(1, 2), waiting(2, 2)]), now);
        let on = |pane| Here::Pane { on: Machine::Local, place: place(pane) };
        let elsewhere = Here::Pane { on: Machine::Remote(nixos()), place: place(2) };
        assert_eq!(alerts.at(Some(&elsewhere)), []);
        let in_a_session = Here::Session { on: Machine::Local, session: "work".to_owned() };
        assert_eq!(alerts.at(Some(&in_a_session)), []);
        let acts = alerts.ticked(now + HELD);
        let mut read = reads(&acts);
        read.sort_by_key(|(pid, _)| *pid);
        assert_eq!(read.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [1, 2]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.at(Some(&on(2))), [key(2)]);
        assert_eq!(closes(&alerts.looked(&alerts.at(Some(&on(2))))), [key(2)]);
        assert_eq!(alerts.looked(&[key(2)]), [], "it went once");

        let later = now + HELD * 2;
        alerts.heard(here(vec![waiting(1, 3), waiting(2, 2)]), later);
        assert_eq!(closes(&alerts.looked(&[key(1), key(9)])), [], "a new wait is held");
        assert_eq!(alerts.ticked(later + HELD), [], "one seen as it was held");
    }

    /// A turn over while a command the agent started in the background runs
    /// on, as Claude Code says `shell` then, says nothing: the turn that
    /// command's end begins says it, once it is over with nothing running.
    /// One whose command ends with no turn after it, as one you stopped,
    /// says nothing either.
    #[test]
    fn a_turn_over_with_a_command_running_on_says_nothing_until_the_agent_is_done() {
        let shell = Agent {
            pid: 1,
            status: Some(Reported::KnownStatus(Status::Shell.into())),
            ..Agent::default()
        };
        assert_eq!(Doing::of(&shell), Doing::Quiet);
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let one = |doing| here(vec![seated(1, doing)]);
        alerts.heard(one(Doing::Busy), now);
        assert_eq!(alerts.heard(one(Doing::of(&shell)), now), []);
        assert_eq!(alerts.heard(one(Doing::Busy), now), [], "its command ended");
        let acts = alerts.heard(one(Doing::Idle), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        shown(&mut alerts, &acts);

        assert_eq!(closes(&alerts.heard(one(Doing::Busy), now)), [key(1)]);
        assert_eq!(alerts.heard(one(Doing::of(&shell)), now), []);
        assert_eq!(alerts.heard(one(Doing::Idle), now), [], "you stopped its command");
    }

    /// No turn that ends is said while a window of luneta's has the focus,
    /// as its row says it there, nor once that window lets the focus go;
    /// and one up goes as a window takes it. A wait is said all the same.
    #[test]
    fn no_turn_that_ends_is_said_while_a_window_of_luneta_s_has_the_focus() {
        let mut alerts = Alerts::new(true);
        let now = Instant::now();
        let two = |one: Doing, two: Doing| here(vec![seated(1, one), seated(2, two)]);
        alerts.heard(two(Doing::Busy, Doing::Busy), now);
        let acts = alerts.heard(two(Doing::Idle, waits(Wait::Permission, 2)), now);
        assert_eq!(acts, []);
        let acts = alerts.ticked(now + HELD);
        assert_eq!(reads(&acts), [(2, Want::Wait(Wait::Permission))]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.focus(false), []);
        assert_eq!(alerts.heard(two(Doing::Idle, waits(Wait::Permission, 2)), now), []);

        alerts.heard(two(Doing::Busy, waits(Wait::Permission, 2)), now);
        let acts = alerts.heard(two(Doing::Idle, waits(Wait::Permission, 2)), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        shown(&mut alerts, &acts);
        assert_eq!(closes(&alerts.focus(true)), [key(1)], "the wait stays up");
        assert_eq!(alerts.focus(true), [], "it went once");
        assert_eq!(alerts.focus(false), []);
        assert_eq!(alerts.heard(two(Doing::Idle, waits(Wait::Permission, 2)), now), []);
    }

    /// Only a wait sounds as it goes up, and not where it was up before; a
    /// turn that ended never does.
    #[test]
    fn only_a_wait_sounds() {
        let sound = |want, again| {
            Alert::of(&Due { again, ..due(want, Machine::Local) }, None).sound
        };
        assert_eq!(sound(Want::Wait(Wait::Input), false), Some(Sound::Question));
        assert_eq!(sound(Want::Wait(Wait::Permission), false), Some(Sound::Bell));
        assert_eq!(sound(Want::Wait(Wait::Unsaid), false), Some(Sound::Bell));
        assert_eq!(sound(Want::Wait(Wait::Permission), true), None);
        assert_eq!(sound(Want::Done, false), None);
    }

    /// A wait up goes up again, without a sound, once this luneta takes over
    /// putting the notifications up; one on its way up goes up as it was, and
    /// what is not up does not. A turn that ended does not, up or on its way.
    #[test]
    fn a_wait_up_goes_up_again_quietly_for_a_luneta_that_takes_over() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pid| seated(pid, Doing::Busy);
        alerts.heard(here(vec![busy(1), busy(2), busy(3), busy(4)]), now);
        let waiting = |pid| seated(pid, waits(Wait::Permission, 2));
        let four = |one| here(vec![seated(1, one), waiting(2), waiting(3), busy(4)]);
        let acts = alerts.heard(four(Doing::Idle), now);
        shown(&mut alerts, &acts);
        let ticked = alerts.ticked(now + HELD);
        let read_for = |pid| {
            let due = |act: &Act| match act {
                Act::Read(due) if due.key.pid == pid => Some(due.clone()),
                Act::Read(_) | Act::Close(_) => None,
            };
            ticked.iter().find_map(due).expect("it is read for")
        };
        let (two, three) = (read_for(2), read_for(3));
        assert!(alerts.read(&read(&two)));

        let acts = alerts.again();
        assert_eq!(reads(&acts), [(2, Want::Wait(Wait::Permission))]);
        for act in &acts {
            let Act::Read(due) = act else { unreachable!("it only reads") };
            assert!(due.again && Alert::of(due, None).sound.is_none(), "{due:?}");
            assert!(alerts.read(&read(due)), "{due:?} goes up");
        }
        assert!(!alerts.read(&read(&two)), "what was read before goes up no more");
        assert!(alerts.read(&read(&three)), "what was on its way up goes up as it was");
        assert_eq!(Alert::of(&three, None).sound, Some(Sound::Bell));
        let acts = alerts.again();
        assert_eq!(reads(&acts).len(), 2, "each wait up goes up again");
        shown(&mut alerts, &acts);

        alerts.heard(four(Doing::Busy), now);
        let acts = alerts.heard(four(Doing::Idle), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        let mut again = reads(&alerts.again());
        again.sort_by_key(|(pid, _)| *pid);
        let wait = Want::Wait(Wait::Permission);
        assert_eq!(again, [(2, wait.clone()), (3, wait)], "nor what is on its way up of a turn");
        let Act::Read(done) = &acts[0] else { unreachable!("it reads for the turn") };
        assert!(!alerts.read(&read(done)));
    }

    /// An agent that ends takes its notification with it; one of a machine
    /// lunetad does not reach just now stays as it was, and one of a machine
    /// it no longer watches goes.
    #[test]
    fn an_agent_that_ends_takes_its_notification_down_and_one_out_of_reach_stays() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let remote = Key { on: Machine::Remote(nixos()), pid: 2 };
        let both = |doing: Doing, there: Reach| {
            let mut machines = here(vec![seated(1, doing)]);
            machines.push((Machine::Remote(nixos()), there));
            machines
        };
        alerts.heard(both(Doing::Busy, heard(vec![seated(2, Doing::Busy)])), now);
        let acts = alerts.heard(both(Doing::Idle, heard(vec![seated(2, Doing::Idle)])), now);
        assert_eq!(reads(&acts), [(1, Want::Done), (2, Want::Done)]);
        shown(&mut alerts, &acts);
        assert_eq!(closes(&alerts.heard(both(Doing::Busy, Reach::Away), now)), [key(1)]);
        let acts = alerts.heard(both(Doing::Busy, heard(vec![seated(2, Doing::Idle)])), now);
        assert_eq!(acts, [], "it is as it was");
        assert_eq!(closes(&alerts.heard(here(Vec::new()), now)), [remote]);
    }

    /// A transcript's end, a record to a line.
    fn end(records: &[Value]) -> String {
        records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
    }

    fn assistant(blocks: &Value) -> Value {
        json!({"type": "assistant", "message": {"content": blocks}})
    }

    fn answer(id: &str) -> Value {
        let block = json!({"type": "tool_result", "tool_use_id": id});
        json!({"type": "user", "message": {"content": [block]}})
    }

    fn call(id: &str, name: &str, input: &Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": name, "input": input})
    }

    fn due(want: Want, on: Machine) -> Due {
        let (key, label) = (Key { on, pid: 2 }, "fix the sidebar".to_owned());
        Due { number: 1, key, want, label, transcript: None, again: false }
    }

    /// A permission names the tool it is for, which has had no answer yet,
    /// and what on; one for a plan says so, as a question does what it asks.
    /// A subagent's call is none of the agent's.
    #[test]
    fn a_notification_says_what_the_agent_wants_by_its_transcript() {
        let mut subagent = assistant(&json!([call("c", "Grep", &json!({"pattern": "x"}))]));
        subagent["isSidechain"] = json!(true);
        let calls = end(&[
            assistant(&json!([
                {"type": "text", "text": "Let me look."},
                call("a", "Read", &json!({"file_path": "/w/x.rs"})),
                call("b", "Bash", &json!({"command": "cargo test"})),
            ])),
            answer("a"),
            subagent,
        ]);
        let said = |wait, end: Option<&str>| {
            let alert = Alert::of(&due(Want::Wait(wait), Machine::Local), end);
            (alert.kind, alert.title, alert.body)
        };
        let title = |headline| format!("fix the sidebar — {headline}");
        assert_eq!(
            said(Wait::Permission, Some(&calls)),
            (Kind::Permission, title("needs permission"), "Bash: cargo test".to_owned())
        );
        let plan = call("p", "ExitPlanMode", &json!({"plan": "1. fix"}));
        let plan = end(&[assistant(&json!([plan]))]);
        assert_eq!(
            said(Wait::Permission, Some(&plan)),
            (Kind::Plan, title("has a plan for you"), "Waiting on your review".to_owned())
        );
        let asked = json!({"questions": [{"question": "Which   one?"}]});
        let question = end(&[assistant(&json!([call("q", "AskUserQuestion", &asked)]))]);
        assert_eq!(
            said(Wait::Input, Some(&question)),
            (Kind::Asked, title("asked you something"), "Which one?".to_owned())
        );
        assert_eq!(
            said(Wait::Permission, None),
            (Kind::Permission, title("needs permission"), "Waiting on your permission".to_owned())
        );
        let blank = json!({"questions": [{"question": "  "}]});
        let blank = end(&[assistant(&json!([call("b", "AskUserQuestion", &blank)]))]);
        assert_eq!(said(Wait::Input, Some(&blank)).2, "Waiting on your answer");
        assert_eq!(said(Wait::Other("sandbox request".to_owned()), None).2, "sandbox request");
        assert_eq!(said(Wait::Unsaid, None).2, "Waiting on you");
    }

    /// A turn that ended says what the agent said last, on one line, cut
    /// short; and of another machine's agent, the machine.
    #[test]
    fn a_turn_that_ended_says_what_the_agent_said_last() {
        let long = "word ".repeat(100);
        let said = end(&[
            assistant(&json!([{"type": "text", "text": "first"}])),
            assistant(&json!([{"type": "text", "text": format!("Done:\n\n{long}")}])),
            json!({"type": "system", "subtype": "turn_duration"}),
        ]);
        let alert = Alert::of(&due(Want::Done, Machine::Remote(nixos())), Some(&said));
        assert_eq!(alert.title, "fix the sidebar on nixos — done");
        assert!(alert.body.starts_with("Done: word word"), "{}", alert.body);
        assert_eq!(alert.body.chars().count(), BODY_CHARS);
        assert!(alert.body.ends_with('…'));
        assert_eq!(Alert::of(&due(Want::Done, Machine::Local), None).body, "Its turn is over");
    }
}
