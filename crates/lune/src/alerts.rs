use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lune_protocol::agent_status::{AgentStatus, BlockedReason};
use lune_protocol::program_status::Message;
use lune_protocol::said::Said;
use luned::api::Agent;
use luned::transcripts::{on_what, one_line, Reading, SessionId, TranscriptEnd, Turn};
use serde_json::Value;

use crate::brief;
use crate::host::Event;
use crate::machines::Machine;
use crate::picker::{Here, Seated};
use crate::sidebar::{Listed, ViewPlace};

pub const HELD: Duration = Duration::from_secs(6);

const SHARE_BYTES: usize = 256 * 1024;

const BODY_CHARS: usize = 150;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub on: Machine,
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    Permission,
    Input,
    Auth,
    Other(Said),
    Unsaid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    Busy,
    Idle,
    Stopped,
    Done { message: Option<Message> },
    Error { message: Option<Message> },
    Waiting { wait: Wait, message: Option<Message>, since: Option<SystemTime> },
    Quiet,
}

impl Doing {
    pub fn of(agent: &Agent) -> Doing {
        match AgentStatus::of_api(agent) {
            AgentStatus::Working => Doing::Busy,
            AgentStatus::Idle => Doing::Idle,
            AgentStatus::Stopped => Doing::Stopped,
            AgentStatus::Done { message } => Doing::Done { message },
            AgentStatus::Error { message } => Doing::Error { message },
            AgentStatus::Blocked { reason, message } => {
                let wait = match reason {
                    BlockedReason::Permission => Wait::Permission,
                    BlockedReason::Question => Wait::Input,
                    BlockedReason::Auth => Wait::Auth,
                    BlockedReason::Dialog => return Doing::Quiet,
                    BlockedReason::Other(word) => Wait::Other(word),
                    BlockedReason::Unspecified => Wait::Unsaid,
                };
                let since = agent.status_since.and_then(|at| at.try_into().ok());
                Doing::Waiting { wait, message, since }
            },
            AgentStatus::Shell | AgentStatus::Other(_) | AgentStatus::Unreported => Doing::Quiet,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    Wait(Wait),
    Done,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub number: u64,
    pub key: Key,
    pub want: Want,
    pub message: Option<Message>,
    pub label: String,
    pub transcript: Option<SessionId>,
    pub again: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Read(Due),
    Close(Key),
}

#[derive(Debug)]
pub enum Reach {
    Heard { agents: Vec<Seated>, listed: Vec<Listed>, unlisted: Vec<Listed> },
    Away,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Told {
    Not,
    Reading(u64),
    Shown,
}

impl Told {
    fn is_up(self) -> bool {
        match self {
            Told::Reading(_) | Told::Shown => true,
            Told::Not => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    Held(Instant),
    Told(Told),
}

#[derive(Debug)]
enum State {
    Busy,
    Idle(Told),
    Stopped,
    Done { message: Option<Message>, told: Told },
    Error { message: Option<Message>, told: Told },
    Waiting { wait: Wait, message: Option<Message>, since: Option<SystemTime>, hold: Hold },
    Quiet,
}

impl State {
    fn new(doing: Doing, now: Instant, as_is: bool) -> State {
        match doing {
            Doing::Busy => State::Busy,
            Doing::Idle => State::Idle(Told::Not),
            Doing::Stopped => State::Stopped,
            Doing::Done { message } => State::Done { message, told: Told::Not },
            Doing::Error { message } => State::Error { message, told: Told::Not },
            Doing::Waiting { wait, message, since } => {
                let hold = if as_is { Hold::Told(Told::Not) } else { Hold::Held(now) };
                State::Waiting { wait, message, since, hold }
            },
            Doing::Quiet => State::Quiet,
        }
    }

    fn is(&self, doing: &Doing) -> bool {
        match (self, doing) {
            (State::Busy, Doing::Busy)
            | (State::Idle(_), Doing::Idle)
            | (State::Stopped, Doing::Stopped)
            | (State::Quiet, Doing::Quiet) => true,
            (State::Done { message, .. }, Doing::Done { message: now })
            | (State::Error { message, .. }, Doing::Error { message: now }) => message == now,
            (
                State::Waiting { wait, message, since, .. },
                Doing::Waiting { wait: now, message: now_message, since: from },
            ) => wait == now && message == now_message && since == from,
            _ => false,
        }
    }

    fn told(&mut self) -> Option<&mut Told> {
        match self {
            State::Idle(told)
            | State::Done { told, .. }
            | State::Error { told, .. }
            | State::Waiting { hold: Hold::Told(told), .. } => Some(told),
            State::Busy | State::Stopped | State::Waiting { hold: Hold::Held(_), .. } | State::Quiet => {
                None
            },
        }
    }

    fn is_up(&self) -> bool {
        match self {
            State::Idle(told)
            | State::Done { told, .. }
            | State::Error { told, .. }
            | State::Waiting { hold: Hold::Told(told), .. } => told.is_up(),
            State::Busy | State::Stopped | State::Waiting { hold: Hold::Held(_), .. } | State::Quiet => {
                false
            },
        }
    }

    fn down(&mut self) {
        match self {
            State::Idle(told) | State::Done { told, .. } | State::Error { told, .. } => {
                *told = Told::Not;
            },
            State::Waiting { hold, .. } => *hold = Hold::Told(Told::Not),
            State::Busy | State::Stopped | State::Quiet => {},
        }
    }
}

#[derive(Debug)]
struct Followed {
    state: State,
    place: ViewPlace,
    label: String,
    transcript: Option<SessionId>,
}

impl Followed {
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
        self.state = match (&self.state, doing) {
            (State::Busy, Doing::Idle) if !focused => {
                *numbered += 1;
                acts.push(Act::Read(self.due(key, *numbered, Want::Done, None, false)));
                State::Idle(Told::Reading(*numbered))
            },
            (State::Busy, Doing::Done { message }) if !focused => {
                *numbered += 1;
                acts.push(Act::Read(self.due(key, *numbered, Want::Done, message.clone(), false)));
                State::Done { message, told: Told::Reading(*numbered) }
            },
            (_, Doing::Error { message }) if !focused => {
                *numbered += 1;
                acts.push(Act::Read(self.due(key, *numbered, Want::Error, message.clone(), false)));
                State::Error { message, told: Told::Reading(*numbered) }
            },
            (_, doing) => State::new(doing, now, false),
        };
        acts
    }

    fn due(
        &self,
        key: &Key,
        number: u64,
        want: Want,
        message: Option<Message>,
        again: bool,
    ) -> Due {
        let (key, label, transcript) = (key.clone(), self.label.clone(), self.transcript.clone());
        Due { number, key, want, message, label, transcript, again }
    }
}

#[derive(Debug, Default)]
pub struct Alerts {
    followed: HashMap<Key, Followed>,
    listed: HashMap<Machine, HashSet<Listed>>,
    numbered: u64,
    focused: bool,
}

impl Alerts {
    pub fn new(focused: bool) -> Alerts {
        Alerts { focused, ..Alerts::default() }
    }

    pub fn focus(&mut self, focused: bool) -> Vec<Act> {
        let took = focused && !self.focused;
        self.focused = focused;
        let mut acts = Vec::new();
        if !took {
            return acts;
        }
        for (key, followed) in &mut self.followed {
            if let State::Idle(told) | State::Done { told, .. } | State::Error { told, .. } =
                &mut followed.state
                && told.is_up()
            {
                acts.push(Act::Close(key.clone()));
                *told = Told::Not;
            }
        }
        acts
    }

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
                    let placed = before.as_ref().is_some_and(|was| was.contains(place.listed()));
                    let state = State::new(doing, now, !placed);
                    self.followed.insert(key.clone(), Followed { state, place, label, transcript });
                }
                running.insert(key);
            }
            self.followed.retain(|key, followed| {
                let ended = key.on == on
                    && !running.contains(key)
                    && !unlisted.contains(followed.place.listed());
                if ended && followed.state.is_up() {
                    acts.push(Act::Close(key.clone()));
                }
                !ended
            });
        }
        acts
    }

    pub fn at(&self, here: Option<&Here>) -> Vec<Key> {
        let Some(Here::View { on, place }) = here else {
            return Vec::new();
        };
        let mut keys: Vec<Key> = (self.followed.iter())
            .filter(|(key, followed)| key.on == *on && followed.place == *place)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by_key(|key| key.pid);
        keys
    }

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

    pub fn due(&self) -> Option<Instant> {
        let held = self.followed.values().filter_map(|followed| match followed.state {
            State::Waiting { hold: Hold::Held(at), .. } => Some(at + HELD),
            State::Busy
            | State::Idle(_)
            | State::Stopped
            | State::Done { .. }
            | State::Error { .. }
            | State::Waiting { hold: Hold::Told(_), .. }
            | State::Quiet => None,
        });
        held.min()
    }

    pub fn ticked(&mut self, now: Instant) -> Vec<Act> {
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            let State::Waiting { wait, message, hold, .. } = &mut followed.state else {
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
            let (want, message) = (Want::Wait(wait.clone()), message.clone());
            acts.push(Act::Read(followed.due(key, self.numbered, want, message, false)));
        }
        acts
    }

    pub fn again(&mut self) -> Vec<Act> {
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            let (wait, message) = match &mut followed.state {
                State::Idle(told) | State::Done { told, .. } | State::Error { told, .. } => {
                    *told = Told::Not;
                    continue;
                },
                State::Waiting { wait, message, hold: Hold::Told(told), .. }
                    if *told == Told::Shown =>
                {
                    self.numbered += 1;
                    *told = Told::Reading(self.numbered);
                    (wait.clone(), message.clone())
                },
                State::Busy | State::Stopped | State::Waiting { .. } | State::Quiet => continue,
            };
            acts.push(Act::Read(followed.due(key, self.numbered, Want::Wait(wait), message, true)));
        }
        acts
    }

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

    pub fn clicked(&mut self, key: &Key) -> Option<ViewPlace> {
        let followed = self.followed.get_mut(key)?;
        followed.state.down();
        Some(followed.place.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Asked,
    Plan,
    Permission,
    Auth,
    Wants,
    Done,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    Question,
    Bell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub key: Key,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub sound: Option<Sound>,
}

impl Alert {
    pub fn of(due: &Due, end: Option<&TranscriptEnd>) -> Alert {
        let turn = end.map_or_default(Turn::of);
        let tool = turn.tool.as_ref().map(|(name, input)| (name.as_str(), input));
        let message = due.message.as_ref().map(Message::as_str);
        let (kind, body) = match (&due.want, message, tool) {
            (Want::Done, Some(message), _) => (Kind::Done, message.to_owned()),
            (Want::Done, None, _) => {
                (Kind::Done, turn.said.unwrap_or_else(|| "Its turn is over".to_owned()))
            },
            (Want::Error, Some(message), _) => (Kind::Error, message.to_owned()),
            (Want::Error, None, _) => (Kind::Error, "Its turn failed".to_owned()),
            (Want::Wait(Wait::Permission), None, Some(("ExitPlanMode", _))) => {
                (Kind::Plan, "Waiting on your review".to_owned())
            },
            (Want::Wait(Wait::Permission), Some(message), _) => {
                (Kind::Permission, message.to_owned())
            },
            (Want::Wait(Wait::Permission), None, tool) => {
                let body = match tool {
                    Some((name, input)) => match run_on(input) {
                        Some(what) => format!("{name}: {what}"),
                        None => name.to_owned(),
                    },
                    None => "Waiting on your permission".to_owned(),
                };
                (Kind::Permission, body)
            },
            (Want::Wait(Wait::Input), Some(message), _) => (Kind::Asked, message.to_owned()),
            (Want::Wait(Wait::Input), None, tool) => {
                let asked = tool.filter(|(name, _)| {
                    matches!(*name, "AskUserQuestion" | "ask_question")
                });
                let question = asked.and_then(|(_, input)| run_on(input));
                (Kind::Asked, question.unwrap_or("Waiting on your answer").to_owned())
            },
            (Want::Wait(Wait::Auth), Some(message), _) => (Kind::Auth, message.to_owned()),
            (Want::Wait(Wait::Auth), None, _) => {
                (Kind::Auth, "Waiting for you to sign in".to_owned())
            },
            (Want::Wait(Wait::Other(_) | Wait::Unsaid), Some(message), _) => {
                (Kind::Wants, message.to_owned())
            },
            (Want::Wait(Wait::Other(word)), None, _) => (Kind::Wants, word.as_str().to_owned()),
            (Want::Wait(Wait::Unsaid), None, _) => (Kind::Wants, "Waiting on you".to_owned()),
        };
        let headline = match kind {
            Kind::Asked => "asked you something",
            Kind::Plan => "has a plan for you",
            Kind::Permission => "needs permission",
            Kind::Auth => "needs sign-in",
            Kind::Wants => "needs you",
            Kind::Done => "done",
            Kind::Error => "failed",
        };
        let title = match &due.key.on {
            Machine::Local => format!("{} — {headline}", due.label),
            Machine::Remote(destination) => format!("{} on {destination} — {headline}", due.label),
        };
        let sound = match kind {
            Kind::Asked | Kind::Plan => Some(Sound::Question),
            Kind::Permission | Kind::Auth | Kind::Wants | Kind::Error => Some(Sound::Bell),
            Kind::Done => None,
        };
        let (key, body) = (due.key.clone(), one_line(&body, BODY_CHARS));
        assert_ne!(title, "");
        assert_ne!(body, "");
        Alert { key, kind, title, body, sound: sound.filter(|_| !due.again) }
    }
}

fn run_on(input: &Value) -> Option<&str> {
    on_what(input).filter(|what| !what.trim().is_empty())
}

#[derive(Debug)]
pub struct Read {
    pub number: u64,
    pub alert: Alert,
}

pub fn read(due: Due, folders: luned::transcripts::Folders, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let end = due.transcript.clone().and_then(|id| {
            let reading = Reading::new(vec![id], SHARE_BYTES).ok()?;
            let ends = brief::read(&due.key.on, &reading, &folders).ok()?;
            ends.into_values().next()
        });
        let alert = Alert::of(&due, end.as_ref());
        let _ = events.send(Event::Alerted(Read { number: due.number, alert }));
    });
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use serde_json::json;

    use luned::api::agent::{ReportedStatus, Status as Reported, WaitingFor as Waits};
    use luned::api::blocked_status::Reason as ApiBlockedReason;
    use luned::api::{BlockedStatus, DoneStatus, ErrorStatus, Status, WaitingFor};
    use luned::Destination;

    use super::*;
    use crate::sidebar::Mark;

    fn work() -> Listed {
        Listed::new_project("work", 10)
    }

    fn place(view: u32) -> ViewPlace {
        ViewPlace::new_in("work", 10, 1, view)
    }

    fn seated(pid: u32, doing: Doing) -> Seated {
        Seated {
            place: place(pid),
            label: format!("agent {pid}"),
            mark: Mark::Unreported,
            status: String::new(),
            since: None,
            dir: "lune/main".to_owned(),
            pid,
            transcript: SessionId::parse(&format!("{pid:08x}-0000-0000-0000-000000000000")),
            doing,
        }
    }

    fn waits(wait: Wait, since: u64) -> Doing {
        Doing::Waiting {
            wait,
            message: None,
            since: Some(UNIX_EPOCH + Duration::from_secs(since)),
        }
    }

    fn heard(agents: Vec<Seated>) -> Reach {
        Reach::Heard { agents, listed: vec![work()], unlisted: Vec::new() }
    }

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

    fn read(due: &Due) -> Read {
        Read { number: due.number, alert: Alert::of(due, None) }
    }

    fn shown(alerts: &mut Alerts, acts: &[Act]) {
        for act in acts {
            if let Act::Read(due) = act {
                assert!(alerts.read(&read(due)), "{due:?} goes up");
            }
        }
    }

    #[test]
    fn what_agents_did_before_lune_heard_of_them_says_nothing() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let first = here(vec![seated(1, waits(Wait::Permission, 5))]);
        assert_eq!(alerts.heard(first, now), []);
        assert_eq!(alerts.due(), None);
        assert_eq!(alerts.ticked(now + HELD), []);
    }

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
        assert_eq!(alerts.ticked(now + HELD), [], "it waited before lune could place it");

        let acts = alerts.heard(here(vec![seated(2, Doing::Idle)]), now);
        assert_eq!(reads(&acts), [(2, Want::Done)]);
        shown(&mut alerts, &acts);
        assert_eq!(alerts.heard(unlisted(), now), [], "its daemon has not placed it yet");
        let again = here(vec![seated(2, Doing::Idle)]);
        assert_eq!(alerts.heard(again, now), [], "it is as it was");
        assert_eq!(closes(&alerts.heard(here(Vec::new()), now)), [key(2)]);
    }

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
        let other = Wait::Other(Said::new("sandbox request".to_owned()).unwrap());
        assert_eq!(Doing::of(&newer), Doing::Waiting { wait: other.clone(), message: None, since: None });
        let blank_other = waiting(4, Some(Waits::OtherWaitingFor("   ".to_owned())));
        assert_eq!(
            Doing::of(&blank_other),
            Doing::Waiting { wait: Wait::Unsaid, message: None, since: None }
        );
        let unsaid = waiting(3, Some(Waits::KnownWaitingFor(99)));
        assert_eq!(
            Doing::of(&unsaid),
            Doing::Waiting { wait: Wait::Unsaid, message: None, since: None }
        );

        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pid| seated(pid, Doing::Busy);
        alerts.heard(here(vec![busy(1), busy(2), busy(3)]), now);
        let all = [(1, &dialog), (2, &newer), (3, &unsaid)];
        alerts.heard(here(all.map(|(pid, agent)| seated(pid, Doing::of(agent))).into()), now);
        let mut read = reads(&alerts.ticked(now + HELD));
        read.sort_by_key(|(pid, _)| *pid);
        assert_eq!(read, [(2, Want::Wait(other)), (3, Want::Wait(Wait::Unsaid))]);
    }

    #[test]
    fn looking_at_an_agent_s_view_takes_its_notification_down() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pid| seated(pid, Doing::Busy);
        alerts.heard(here(vec![busy(1), busy(2)]), now);
        let waiting = |pid, since| seated(pid, waits(Wait::Input, since));
        alerts.heard(here(vec![waiting(1, 2), waiting(2, 2)]), now);
        let on = |view| Here::View { on: Machine::Local, place: place(view) };
        let elsewhere = Here::View { on: Machine::Remote(nixos()), place: place(2) };
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

    #[test]
    fn no_turn_that_ends_is_said_while_a_window_of_lune_s_has_the_focus() {
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

    #[test]
    fn a_wait_up_goes_up_again_quietly_for_a_lune_that_takes_over() {
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

    fn claude_end(records: &[Value]) -> TranscriptEnd {
        TranscriptEnd::Claude(records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"))
    }

    fn agy_end(records: &[Value]) -> TranscriptEnd {
        TranscriptEnd::Antigravity(
            records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"),
        )
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
        Due { number: 1, key, want, message: None, label, transcript: None, again: false }
    }

    #[test]
    fn a_notification_says_what_the_agent_wants_by_its_transcript() {
        let mut subagent = assistant(&json!([call("c", "Grep", &json!({"pattern": "x"}))]));
        subagent["isSidechain"] = json!(true);
        let calls = claude_end(&[
            assistant(&json!([
                {"type": "text", "text": "Let me look."},
                call("a", "Read", &json!({"file_path": "/w/x.rs"})),
                call("b", "Bash", &json!({"command": "cargo test"})),
            ])),
            answer("a"),
            subagent,
        ]);
        let said = |wait, end: Option<&TranscriptEnd>| {
            let alert = Alert::of(&due(Want::Wait(wait), Machine::Local), end);
            (alert.kind, alert.title, alert.body)
        };
        let title = |headline| format!("fix the sidebar — {headline}");
        assert_eq!(
            said(Wait::Permission, Some(&calls)),
            (Kind::Permission, title("needs permission"), "Bash: cargo test".to_owned())
        );
        let plan = call("p", "ExitPlanMode", &json!({"plan": "1. fix"}));
        let plan = claude_end(&[assistant(&json!([plan]))]);
        assert_eq!(
            said(Wait::Permission, Some(&plan)),
            (Kind::Plan, title("has a plan for you"), "Waiting on your review".to_owned())
        );
        let asked = json!({"questions": [{"question": "Which   one?"}]});
        let question = claude_end(&[assistant(&json!([call("q", "AskUserQuestion", &asked)]))]);
        assert_eq!(
            said(Wait::Input, Some(&question)),
            (Kind::Asked, title("asked you something"), "Which one?".to_owned())
        );
        assert_eq!(
            said(Wait::Permission, None),
            (Kind::Permission, title("needs permission"), "Waiting on your permission".to_owned())
        );
        let blank = json!({"questions": [{"question": "  "}]});
        let blank = claude_end(&[assistant(&json!([call("b", "AskUserQuestion", &blank)]))]);
        assert_eq!(said(Wait::Input, Some(&blank)).2, "Waiting on your answer");
        assert_eq!(
            said(Wait::Other(Said::new("sandbox request".to_owned()).unwrap()), None).2,
            "sandbox request"
        );
        assert_eq!(said(Wait::Unsaid, None).2, "Waiting on you");
    }

    #[test]
    fn a_turn_that_ended_says_what_the_agent_said_last() {
        let long = "word ".repeat(100);
        let said = claude_end(&[
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

    #[test]
    fn an_antigravity_transcript_fills_permission_question_and_done_notifications() {
        let perm = agy_end(&[json!({
            "type": "PLANNER_RESPONSE",
            "tool_calls": [{
                "name": "run_command",
                "args": {"CommandLine": "\"cargo test\"", "toolSummary": "\"Run tests\""}
            }]
        })]);
        let alert = Alert::of(&due(Want::Wait(Wait::Permission), Machine::Local), Some(&perm));
        assert_eq!((alert.kind, alert.body.as_str()), (Kind::Permission, "run_command: Run tests"));

        let ask = agy_end(&[json!({
            "type": "PLANNER_RESPONSE",
            "tool_calls": [{
                "name": "ask_question",
                "args": {"questions": "[{\"question\":\"Which harness?\"}]"}
            }]
        })]);
        let alert = Alert::of(&due(Want::Wait(Wait::Input), Machine::Local), Some(&ask));
        assert_eq!((alert.kind, alert.body.as_str()), (Kind::Asked, "Which harness?"));

        let done = agy_end(&[
            json!({"type": "USER_INPUT", "source": "USER_EXPLICIT", "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>"}),
            json!({"type": "PLANNER_RESPONSE", "content": "All tests pass."}),
        ]);
        let alert = Alert::of(&due(Want::Done, Machine::Local), Some(&done));
        assert_eq!((alert.kind, alert.body.as_str()), (Kind::Done, "All tests pass."));
    }

    #[test]
    fn osc_7501_done_error_auth_and_interrupted_idle_behave_distinctly() {
        use luned::api::StatusRung;

        let osc_agent = |reported_status: ReportedStatus| Agent {
            pid: 1,
            reported_status: Some(reported_status),
            status_rung: Some(StatusRung::Osc7501.into()),
            ..Agent::default()
        };

        let stopped = osc_agent(ReportedStatus::Stopped(()));
        assert_eq!(Doing::of(&stopped), Doing::Stopped);
        let mut alerts = Alerts::default();
        let now = Instant::now();
        alerts.heard(here(vec![seated(1, Doing::Busy)]), now);
        assert_eq!(alerts.heard(here(vec![seated(1, Doing::of(&stopped))]), now), []);

        let done = osc_agent(ReportedStatus::Done(DoneStatus {
            message: Some("Fixed the bug.".to_owned()),
        }));
        alerts.heard(here(vec![seated(1, Doing::Busy)]), now);
        let acts = alerts.heard(here(vec![seated(1, Doing::of(&done))]), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        let Act::Read(due_done) = &acts[0] else { unreachable!() };
        let alert = Alert::of(due_done, None);
        assert_eq!((alert.kind, alert.body.as_str()), (Kind::Done, "Fixed the bug."));

        let err = osc_agent(ReportedStatus::Error(ErrorStatus {
            message: Some("API rate limit".to_owned()),
        }));
        let acts = alerts.heard(here(vec![seated(1, Doing::of(&err))]), now);
        assert_eq!(reads(&acts), [(1, Want::Error)]);
        let [Act::Close(_), Act::Read(due_err)] = acts.as_slice() else { unreachable!() };
        let alert = Alert::of(due_err, None);
        assert_eq!(
            (alert.kind, alert.title.as_str(), alert.body.as_str(), alert.sound),
            (Kind::Error, "agent 1 — failed", "API rate limit", Some(Sound::Bell))
        );

        let auth = osc_agent(ReportedStatus::Blocked(BlockedStatus {
            reason: Some(ApiBlockedReason::Known(WaitingFor::Auth.into())),
            message: Some("Sign in to GitHub".to_owned()),
        }));
        alerts.heard(here(vec![seated(1, Doing::of(&auth))]), now);
        let acts = alerts.ticked(now + HELD);
        assert_eq!(reads(&acts), [(1, Want::Wait(Wait::Auth))]);
        let Act::Read(due_auth) = &acts[0] else { unreachable!() };
        let alert = Alert::of(due_auth, None);
        assert_eq!(
            (alert.kind, alert.title.as_str(), alert.body.as_str(), alert.sound),
            (Kind::Auth, "agent 1 — needs sign-in", "Sign in to GitHub", Some(Sound::Bell))
        );
    }
}
