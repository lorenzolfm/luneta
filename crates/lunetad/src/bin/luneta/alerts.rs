//! The desktop's notifications of the agents that want you, of this machine
//! and of each other that lunetad reaches: of one that waits on you, once it
//! has waited `HELD`, and of one done with its turn, at once. Each goes once
//! its agent no longer wants you, or once you look at its pane, as you do
//! while luneta's window has the focus and its zellij is on that pane. None
//! comes of a pane you look at.
//!
//! What a notification says is in the end of its agent's transcript, which
//! is read before it goes up: the tool a permission is for, which tells a
//! plan from a command, the question asked, or what the agent said last. An
//! agent that no longer wants you by the time it is read has none.
//!
//! A machine lunetad does not reach just now keeps its agents as they were,
//! and their notifications, until it is heard again.

use std::collections::{HashMap, HashSet};
use std::iter;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lunetad::transcripts::Reading;
use serde_json::Value;

use crate::brief::{self, on_what, one_line};
use crate::host::Event;
use crate::machines::Machine;
use crate::picker::{Here, Listing, Seated};
use crate::sidebar::{Mark, PanePlace};

/// How long an agent waits on you before the desktop says so: as long as
/// Claude Code waits before it tells its hooks, so that a dialog that goes
/// as it came, as one auto mode answers itself, says nothing.
pub const HELD: Duration = Duration::from_secs(6);

/// What Claude Code waits for, in its word, while one of its own dialogs is
/// open, as a command of its opens: you opened it, and it says nothing.
const YOURS: &str = "dialog open";

/// How much of the end of a transcript a notification reads: the records of
/// the last turn, which a tool's long answer can be most of.
const SHARE_BYTES: u32 = 256 * 1024;

/// The longest a notification's body is, on one line: enough to carry a
/// thought, short enough that the desktop does not cut it.
const BODY_CHARS: usize = 150;

/// An agent, by the machine it runs on and Claude Code's id for its session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub on: Machine,
    pub id: String,
}

/// What an agent wants of you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// It waits on you, for what Claude Code says it waits for, where it says.
    Wait(Option<String>),
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
}

/// What the desktop is to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Read the end of the agent's transcript, for the notification `Due` is.
    Read(Due),
    /// Take the agent's notification down, where one is up.
    Close(Key),
}

/// What an agent does, as last heard.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Doing {
    Busy,
    Idle,
    /// It waits on you, for what Claude Code says, since when it says: a wait
    /// since another moment is another wait.
    Waiting { word: Option<String>, since: Option<SystemTime> },
    /// What is neither, as a word luneta does not know.
    Else,
}

impl Doing {
    fn of(seated: &Seated) -> Doing {
        match seated.mark {
            Mark::Busy => Doing::Busy,
            // One finished since you were last in its pane is as done.
            Mark::Idle | Mark::Finished => Doing::Idle,
            Mark::Waiting => {
                Doing::Waiting { word: seated.waiting_for.clone(), since: seated.since }
            },
            Mark::Shell
            | Mark::Other(_)
            | Mark::Unreported
            | Mark::Passed
            | Mark::Failed
            | Mark::Ran => Doing::Else,
        }
    }
}

/// Where an agent's notification is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Up {
    /// None is up, nor on its way.
    Down,
    /// It waits on you since luneta heard so, at this instant, and its
    /// notification goes up once it has waited `HELD`.
    Held(Instant),
    /// The end of its transcript is being read, for the notification of this
    /// number.
    Reading(u64),
    Shown,
}

/// An agent luneta follows.
#[derive(Debug)]
struct Followed {
    doing: Doing,
    place: PanePlace,
    label: String,
    up: Up,
}

/// The agents of every machine luneta heard of, and their notifications.
#[derive(Debug, Default)]
pub struct Alerts {
    followed: HashMap<Key, Followed>,
    /// The machines heard of once at least. The agents of one heard of the
    /// first time are taken as they are: what they did before, and whether
    /// you were told, luneta cannot say.
    heard: HashSet<Machine>,
    /// The number of the last notification read for.
    numbered: u64,
}

impl Alerts {
    /// Takes in what runs on each machine, as `listing` says, at `now`: what
    /// the desktop is to do, where an agent came to wait on you, or is done,
    /// or does anything else since, or ended. A machine left out of
    /// `listing`, which lunetad does not reach, keeps its agents as they
    /// were.
    pub fn heard(&mut self, listing: &Listing, now: Instant) -> Vec<Act> {
        let remotes = listing.remotes.iter();
        let remotes = remotes.map(|(to, runs)| (Machine::Remote(to.clone()), runs));
        let mut acts = Vec::new();
        for (on, runs) in iter::once((Machine::Local, &listing.local)).chain(remotes) {
            let first = self.heard.insert(on.clone());
            let mut running = HashSet::with_capacity(runs.agents.len());
            for seated in &runs.agents {
                let key = Key { on: on.clone(), id: seated.id.clone() };
                let doing = Doing::of(seated);
                if let Some(followed) = self.followed.get_mut(&key) {
                    followed.place.clone_from(&seated.place);
                    followed.label.clone_from(&seated.label);
                    acts.extend(followed.does(&key, doing, now, &mut self.numbered));
                } else {
                    let up = match &doing {
                        Doing::Waiting { word, .. } if !first && says(word.as_deref()) => {
                            Up::Held(now)
                        },
                        Doing::Waiting { .. } | Doing::Busy | Doing::Idle | Doing::Else => Up::Down,
                    };
                    let (place, label) = (seated.place.clone(), seated.label.clone());
                    self.followed.insert(key.clone(), Followed { doing, place, label, up });
                }
                running.insert(key);
            }
            self.followed.retain(|key, followed| {
                let ended = key.on == on && !running.contains(key);
                if ended && followed.is_up() {
                    acts.push(Act::Close(key.clone()));
                }
                !ended
            });
        }
        acts
    }

    /// You are where `here` says, in luneta's window, which has the focus
    /// where `focused` says: the notification of an agent in the pane you
    /// look at goes, and one held for it never goes up.
    pub fn looked(&mut self, here: Option<&Here>, focused: bool) -> Vec<Act> {
        let Some(Here::Pane { on, place }) = here.filter(|_| focused) else {
            return Vec::new();
        };
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            if key.on != *on || followed.place != *place {
                continue;
            }
            if followed.is_up() {
                acts.push(Act::Close(key.clone()));
            }
            followed.up = Up::Down;
        }
        acts
    }

    /// When the next agent that waits on you has waited `HELD`, if one does.
    pub fn due(&self) -> Option<Instant> {
        let held = self.followed.values().filter_map(|followed| match followed.up {
            Up::Held(at) => Some(at + HELD),
            Up::Down | Up::Reading(_) | Up::Shown => None,
        });
        held.min()
    }

    /// The notifications of the agents that have waited on you `HELD` by
    /// `now`, to read for.
    pub fn ticked(&mut self, now: Instant) -> Vec<Act> {
        let mut acts = Vec::new();
        for (key, followed) in &mut self.followed {
            let (Up::Held(at), Doing::Waiting { word, .. }) = (followed.up, &followed.doing) else {
                continue;
            };
            if now < at + HELD {
                continue;
            }
            self.numbered += 1;
            followed.up = Up::Reading(self.numbered);
            let want = Want::Wait(word.clone());
            let label = followed.label.clone();
            acts.push(Act::Read(Due { number: self.numbered, key: key.clone(), want, label }));
        }
        acts
    }

    /// The notification numbered `number` was read for: whether it goes up,
    /// as it does while its agent still wants what it was read for, and you
    /// have not looked.
    pub fn read(&mut self, number: u64) -> bool {
        let mut followed = self.followed.values_mut();
        let Some(followed) = followed.find(|followed| followed.up == Up::Reading(number)) else {
            return false;
        };
        followed.up = Up::Shown;
        true
    }

    /// You clicked the notification of `key`'s agent, which goes: where its
    /// pane is, as last heard, if luneta still follows it.
    pub fn clicked(&mut self, key: &Key) -> Option<PanePlace> {
        let followed = self.followed.get_mut(key)?;
        followed.up = Up::Down;
        Some(followed.place.clone())
    }
}

impl Followed {
    /// Whether a notification is up, or on its way up.
    fn is_up(&self) -> bool {
        match self.up {
            Up::Reading(_) | Up::Shown => true,
            Up::Down | Up::Held(_) => false,
        }
    }

    /// The agent of `key` does as `doing` says, as heard at `now`: what the
    /// desktop is to do of it. Whatever it does since makes what was up of it
    /// stale. A wait that begins is held; and a turn that ends is said at
    /// once, numbered after `numbered`.
    fn does(&mut self, key: &Key, doing: Doing, now: Instant, numbered: &mut u64) -> Vec<Act> {
        if doing == self.doing {
            return Vec::new();
        }
        let mut acts = Vec::new();
        if self.is_up() {
            acts.push(Act::Close(key.clone()));
        }
        let ended = self.doing == Doing::Busy && doing == Doing::Idle;
        self.up = match &doing {
            Doing::Waiting { word, .. } if says(word.as_deref()) => Up::Held(now),
            Doing::Idle if ended => {
                *numbered += 1;
                let (number, key, label) = (*numbered, key.clone(), self.label.clone());
                acts.push(Act::Read(Due { number, key, want: Want::Done, label }));
                Up::Reading(number)
            },
            Doing::Waiting { .. } | Doing::Busy | Doing::Idle | Doing::Else => Up::Down,
        };
        self.doing = doing;
        acts
    }
}

/// Whether a wait for `word` is one to say.
fn says(word: Option<&str>) -> bool {
    word != Some(YOURS)
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

/// A notification, as it goes up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub key: Key,
    pub kind: Kind,
    pub title: String,
    pub body: String,
}

impl Alert {
    /// The notification `due` is, by the end of its agent's transcript,
    /// `end`, where it was read.
    pub fn of(due: &Due, end: Option<&str>) -> Alert {
        let turn = end.map(Turn::of).unwrap_or_default();
        let tool = turn.tool.as_ref().map(|(name, input)| (name.as_str(), input));
        let (kind, body) = match (&due.want, tool) {
            (Want::Done, _) => {
                (Kind::Done, turn.said.unwrap_or_else(|| "Its turn is over".to_owned()))
            },
            (Want::Wait(Some(word)), Some(("ExitPlanMode", _))) if word == "permission prompt" => {
                (Kind::Plan, "Waiting on your review".to_owned())
            },
            (Want::Wait(Some(word)), tool) if word == "permission prompt" => {
                let body = match tool {
                    Some((name, input)) => match on_what(input) {
                        Some(what) => format!("{name}: {what}"),
                        None => name.to_owned(),
                    },
                    None => "Waiting on your permission".to_owned(),
                };
                (Kind::Permission, body)
            },
            (Want::Wait(Some(word)), tool) if word == "input needed" => {
                let asked = tool.filter(|(name, _)| *name == "AskUserQuestion");
                let question = asked.and_then(|(_, input)| on_what(input));
                (Kind::Asked, question.unwrap_or("Waiting on your answer").to_owned())
            },
            (Want::Wait(Some(word)), _) => (Kind::Wants, word.clone()),
            (Want::Wait(None), _) => (Kind::Wants, "Waiting on you".to_owned()),
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
        Alert { key: due.key.clone(), kind, title, body: one_line(&body, BODY_CHARS) }
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
/// back the notification it makes of it. One whose end does not read says
/// what `due` does alone.
pub fn read(due: Due, claude_folder: Option<PathBuf>, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let projects = claude_folder.map(|folder| folder.join("projects"));
        let ends = Reading::of(std::slice::from_ref(&due.key.id), SHARE_BYTES)
            .and_then(|reading| brief::read(&due.key.on, &reading, projects.as_deref()));
        let end = ends.ok().and_then(|ends| ends.into_values().next());
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
    use crate::picker::Runs;
    use crate::sidebar::Listed;

    /// An agent in pane `pane` of the session `work`, as the picker lists it.
    fn seated(pane: u32, mark: Mark, waiting_for: Option<&str>, since: u64) -> Seated {
        Seated {
            place: place(pane),
            label: format!("agent {pane}"),
            mark,
            status: String::new(),
            since: Some(UNIX_EPOCH + Duration::from_secs(since)),
            dir: "luneta/main".to_owned(),
            id: format!("id-{pane}"),
            waiting_for: waiting_for.map(str::to_owned),
        }
    }

    fn place(pane: u32) -> PanePlace {
        PanePlace { session: Listed::new("work", UNIX_EPOCH + Duration::from_secs(10)), pane }
    }

    fn runs(agents: Vec<Seated>) -> Runs {
        Runs { sessions: Vec::new(), agents }
    }

    /// This machine's agents, and no other machine.
    fn listing(agents: Vec<Seated>) -> Listing {
        Listing { local: runs(agents), ..Listing::default() }
    }

    fn nixos() -> Destination {
        "nixos".parse().expect("a destination")
    }

    fn key(pane: u32) -> Key {
        Key { on: Machine::Local, id: format!("id-{pane}") }
    }

    fn reads(acts: &[Act]) -> Vec<(u32, Want)> {
        let pane = |due: &Due| due.key.id.trim_start_matches("id-").parse().expect("a pane");
        let read = |act: &Act| match act {
            Act::Read(due) => Some((pane(due), due.want.clone())),
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

    /// The agents luneta hears of first are as they are: one that waited
    /// already, or finished, says nothing, as luneta cannot say whether you
    /// were told.
    #[test]
    fn what_agents_did_before_luneta_heard_of_them_says_nothing() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let first = listing(vec![seated(1, Mark::Waiting, Some("permission prompt"), 5)]);
        assert_eq!(alerts.heard(&first, now), []);
        assert_eq!(alerts.due(), None);
        assert_eq!(alerts.ticked(now + HELD), []);
    }

    /// A wait goes up once it has lasted `HELD`, and one over before then,
    /// as a dialog auto mode answers itself, never does. A wait that ends
    /// takes its notification down.
    #[test]
    fn a_wait_goes_up_once_it_has_lasted_and_down_once_it_ends() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        alerts.heard(&listing(vec![seated(1, Mark::Busy, None, 1)]), now);

        let waits = listing(vec![seated(1, Mark::Waiting, Some("permission prompt"), 2)]);
        assert_eq!(alerts.heard(&waits, now), []);
        assert_eq!(alerts.due(), Some(now + HELD));
        assert_eq!(alerts.ticked(now + HELD.saturating_sub(Duration::from_millis(1))), []);
        let works = listing(vec![seated(1, Mark::Busy, None, 3)]);
        assert_eq!(alerts.heard(&works, now + Duration::from_secs(1)), []);
        assert_eq!(alerts.due(), None);

        let later = now + Duration::from_secs(2);
        alerts.heard(&listing(vec![seated(1, Mark::Waiting, Some("input needed"), 4)]), later);
        let acts = alerts.ticked(later + HELD);
        assert_eq!(reads(&acts), [(1, Want::Wait(Some("input needed".to_owned())))]);
        let Act::Read(due) = &acts[0] else { unreachable!() };
        assert!(alerts.read(due.number));
        assert_eq!(alerts.ticked(later + HELD * 2), [], "it goes up once");
        let acts = alerts.heard(&listing(vec![seated(1, Mark::Busy, None, 5)]), later + HELD * 2);
        assert_eq!(closes(&acts), [key(1)]);
    }

    /// A turn that ends is said at once; a wait that ends in idle, as one
    /// you said no to, is not. A turn said goes once the agent works again.
    #[test]
    fn a_turn_that_ends_is_said_at_once_and_goes_once_the_agent_works_again() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let two = |one: Mark, since, two: Mark| {
            listing(vec![seated(1, one, None, since), seated(2, two, None, 1)])
        };
        alerts.heard(&two(Mark::Busy, 1, Mark::Waiting), now);
        let acts = alerts.heard(&two(Mark::Finished, 2, Mark::Idle), now);
        assert_eq!(reads(&acts), [(1, Want::Done)]);
        let Act::Read(due) = &acts[0] else { unreachable!() };
        assert!(alerts.read(due.number));
        let acts = alerts.heard(&two(Mark::Idle, 2, Mark::Idle), now);
        assert_eq!(acts, [], "seen since it finished, it is as done");
        let acts = alerts.heard(&two(Mark::Busy, 3, Mark::Idle), now);
        assert_eq!(closes(&acts), [key(1)]);
    }

    /// What was read for an agent that no longer wants it by the time it is
    /// read does not go up, nor does what was read before a newer reading.
    #[test]
    fn what_is_read_too_late_does_not_go_up() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        alerts.heard(&listing(vec![seated(1, Mark::Busy, None, 1)]), now);
        let acts = alerts.heard(&listing(vec![seated(1, Mark::Idle, None, 2)]), now);
        let Act::Read(done) = &acts[0] else { unreachable!() };
        let waits = listing(vec![seated(1, Mark::Waiting, Some("input needed"), 3)]);
        let acts = alerts.heard(&waits, now);
        assert_eq!(closes(&acts), [key(1)], "what is on its way up is taken down too");
        assert!(!alerts.read(done.number));
        let acts = alerts.ticked(now + HELD);
        let Act::Read(wait) = &acts[0] else { unreachable!() };
        alerts.heard(&listing(vec![seated(1, Mark::Busy, None, 4)]), now + HELD);
        assert!(!alerts.read(wait.number));
    }

    /// A dialog of Claude Code's own you opened says nothing; any other
    /// wait, of a word luneta does not know too, does.
    #[test]
    fn a_dialog_you_opened_says_nothing() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pane| seated(pane, Mark::Busy, None, 1);
        alerts.heard(&listing(vec![busy(1), busy(2), busy(3)]), now);
        let waits = |pane, word| seated(pane, Mark::Waiting, word, 2);
        let all = [waits(1, Some("dialog open")), waits(2, Some("a newer word")), waits(3, None)];
        alerts.heard(&listing(all.into()), now);
        let mut read = reads(&alerts.ticked(now + HELD));
        read.sort_by_key(|(pane, _)| *pane);
        assert_eq!(read, [(2, Want::Wait(Some("a newer word".to_owned()))), (3, Want::Wait(None))]);
    }

    /// The notification of an agent in the pane you look at goes, and one
    /// held for it never goes up: but only while luneta's window has the
    /// focus, and of that machine's pane.
    #[test]
    fn looking_at_an_agent_s_pane_takes_its_notification_down() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let busy = |pane| seated(pane, Mark::Busy, None, 1);
        alerts.heard(&listing(vec![busy(1), busy(2)]), now);
        let waits = |pane, since| seated(pane, Mark::Waiting, Some("input needed"), since);
        alerts.heard(&listing(vec![waits(1, 2), waits(2, 2)]), now);
        let on = |pane| Here::Pane { on: Machine::Local, place: place(pane) };
        assert_eq!(alerts.looked(Some(&on(1)), false), []);
        let elsewhere = Here::Pane { on: Machine::Remote(nixos()), place: place(2) };
        assert_eq!(alerts.looked(Some(&elsewhere), true), []);
        let acts = alerts.ticked(now + HELD);
        let mut read = reads(&acts);
        read.sort_by_key(|(pane, _)| *pane);
        assert_eq!(read.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(), [1, 2]);
        for act in &acts {
            let Act::Read(due) = act else { unreachable!() };
            assert!(alerts.read(due.number));
        }
        assert_eq!(closes(&alerts.looked(Some(&on(2)), true)), [key(2)]);
        assert_eq!(alerts.looked(Some(&on(2)), true), [], "it went once");

        let later = now + HELD * 2;
        alerts.heard(&listing(vec![waits(1, 3), waits(2, 2)]), later);
        assert_eq!(closes(&alerts.looked(Some(&on(1)), true)), [], "a new wait is held");
        assert_eq!(alerts.ticked(later + HELD), [], "one seen as it was held");
    }

    /// An agent that ends takes its notification with it; one of a machine
    /// lunetad does not reach just now stays as it was.
    #[test]
    fn an_agent_that_ends_takes_its_notification_down_and_one_out_of_reach_stays() {
        let mut alerts = Alerts::default();
        let now = Instant::now();
        let both = |mark: Mark, since| Listing {
            remotes: vec![(nixos(), runs(vec![seated(2, mark.clone(), None, since)]))],
            ..listing(vec![seated(1, mark, None, since)])
        };
        alerts.heard(&both(Mark::Busy, 1), now);
        for act in alerts.heard(&both(Mark::Idle, 2), now) {
            let Act::Read(due) = act else { panic!("{act:?} reads nothing") };
            assert!(alerts.read(due.number));
        }
        let unreached = listing(Vec::new());
        assert_eq!(closes(&alerts.heard(&unreached, now)), [key(1)]);
        let gone = Listing { remotes: vec![(nixos(), runs(Vec::new()))], ..listing(Vec::new()) };
        let remote_key = Key { on: Machine::Remote(nixos()), id: "id-2".to_owned() };
        assert_eq!(closes(&alerts.heard(&gone, now)), [remote_key]);
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
        let (key, label) = (Key { on, id: "id-1".to_owned() }, "fix the sidebar".to_owned());
        Due { number: 1, key, want, label }
    }

    fn wait(word: &str) -> Want {
        Want::Wait(Some(word.to_owned()))
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
        let said = |want, end: Option<&str>| {
            let alert = Alert::of(&due(want, Machine::Local), end);
            (alert.kind, alert.title, alert.body)
        };
        let title = |headline| format!("fix the sidebar — {headline}");
        assert_eq!(
            said(wait("permission prompt"), Some(&calls)),
            (Kind::Permission, title("needs permission"), "Bash: cargo test".to_owned())
        );
        let plan = call("p", "ExitPlanMode", &json!({"plan": "1. fix"}));
        let plan = end(&[assistant(&json!([plan]))]);
        assert_eq!(
            said(wait("permission prompt"), Some(&plan)),
            (Kind::Plan, title("has a plan for you"), "Waiting on your review".to_owned())
        );
        let asked = json!({"questions": [{"question": "Which   one?"}]});
        let question = end(&[assistant(&json!([call("q", "AskUserQuestion", &asked)]))]);
        assert_eq!(
            said(wait("input needed"), Some(&question)),
            (Kind::Asked, title("asked you something"), "Which one?".to_owned())
        );
        assert_eq!(
            said(wait("permission prompt"), None),
            (Kind::Permission, title("needs permission"), "Waiting on your permission".to_owned())
        );
        assert_eq!(said(wait("sandbox request"), None).2, "sandbox request");
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
