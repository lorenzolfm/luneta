use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyModifiers};
use lunetad::api::say::Does;
use lunetad::api::{Saying, Utterance};
use lunetad::sayings::STALE;
use lunetad::transcripts::one_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::brief::Asking;
use crate::host::Event;
use crate::machines::{self, Machine};
use crate::models::Fetched;
use crate::sidebar::{cut, ACCENT, NAME};
use crate::spots::Spots;
use crate::voice::{self, Reading, Voice};

pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('u'), KeyModifiers::ALT);

pub const STOPS: (KeyCode, KeyModifiers) =
    (KeyCode::Char('u'), KeyModifiers::ALT.union(KeyModifiers::SHIFT));

pub fn stops((code, modifiers): (KeyCode, KeyModifiers)) -> bool {
    matches!(code, KeyCode::Char('u' | 'U')) && modifiers == STOPS.1
}

pub const MARK: &str = "\u{f057e}";

pub const PAUSE: &str = "\u{f03e4}";

pub const PLAY: &str = "\u{f040a}";

pub const STOP: &str = "\u{f04db}";

const BUTTON: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Pause,
    GoOn,
    Stop,
}

impl Button {
    fn icon(self) -> &'static str {
        match self {
            Button::Pause => PAUSE,
            Button::GoOn => PLAY,
            Button::Stop => STOP,
        }
    }
}

const WAITING: usize = 16;

const SHOWN_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub enum Whose {
    Selection,
    Said { on: Machine },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Reading { whose: Whose, text: String, paused: bool },
    Fetching { got: u64, paused: bool },
}

impl Shown {
    pub fn paused(&self) -> bool {
        match self {
            Shown::Reading { paused, .. } | Shown::Fetching { paused, .. } => *paused,
        }
    }

    pub fn line(&self, width: usize) -> (Line<'static>, Spots<Button>) {
        let room = width.saturating_sub(2 * BUTTON);
        let mut line = self.said(room);
        let toggle = if self.paused() { Button::GoOn } else { Button::Pause };
        let said = drawn_width(&line, width);
        let from = room.max(said);
        let filler = " ".repeat(from - said);
        let buttons = format!("{filler} {}  {} ", toggle.icon(), Button::Stop.icon());
        line.spans.push(Span::styled(buttons, Style::new().fg(ACCENT)));
        let mut spots = Spots::default();
        for (at, button) in [(from, toggle), (from + BUTTON, Button::Stop)] {
            if at + 1 < width {
                spots.add(0, at..at + BUTTON, button);
            }
        }
        (line, spots)
    }

    fn said(&self, width: usize) -> Line<'static> {
        let mark = Span::styled(MARK, Style::new().fg(ACCENT));
        let room = width.saturating_sub(2);
        let dim = Style::new().add_modifier(Modifier::DIM);
        match self {
            Shown::Fetching { got, .. } => {
                let (got, of) = (got / 1_000_000, voice::BYTES / 1_000_000);
                let said = cut(&format!("voice: {got} of {of} MB"), room);
                Line::from(vec![mark, Span::raw(" "), Span::styled(said, dim)])
            },
            Shown::Reading { whose, text, .. } => {
                let who = match whose {
                    Whose::Selection => "selection".to_owned(),
                    Whose::Said { on: Machine::Local } => machines::local_name(),
                    Whose::Said { on: Machine::Remote(on) } => on.to_string(),
                };
                let who = cut(&who, room);
                let rest = room.saturating_sub(who.width() + 3);
                let name = Span::styled(who, Style::new().fg(NAME));
                let mut said = vec![mark, Span::raw(" "), name];
                if rest > 0 {
                    said.push(Span::styled(format!(" · {}", cut(text, rest)), dim));
                }
                Line::from(said)
            },
        }
    }
}

fn drawn_width(line: &Line<'_>, width: usize) -> usize {
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    usize::from(buffer.set_line(0, 0, line, width).0)
}

#[derive(Debug)]
struct Waiting {
    whose: Whose,
    text: String,
    came: Option<SystemTime>,
}

impl Waiting {
    fn stale(&self, now: SystemTime) -> bool {
        self.came.is_some_and(|came| now.duration_since(came).is_ok_and(|age| age > STALE))
    }
}

#[derive(Debug)]
struct Now {
    number: u64,
    whose: Whose,
    text: String,
    _reading: Reading,
}

#[derive(Debug, PartialEq)]
struct Next {
    spoken: String,
    whose: Whose,
    text: String,
}

#[derive(Debug, Default)]
struct Queue {
    at_once: Option<Waiting>,
    in_turn: VecDeque<Waiting>,
    now: Option<Now>,
    numbered: u64,
    heard: HashMap<Machine, Option<(u32, u64)>>,
    paused: bool,
}

impl Queue {
    fn hear(&mut self, machines: Vec<(Machine, Option<Vec<Saying>>)>, read: bool, at: SystemTime) {
        let watched: HashSet<&Machine> = machines.iter().map(|(on, _)| on).collect();
        self.heard.retain(|on, _| watched.contains(on));
        for (on, sayings) in machines {
            let Some(sayings) = sayings else {
                continue;
            };
            let last = sayings.last().map(|last| (last.pid, last.number));
            let Some(&seen) = self.heard.get(&on) else {
                self.heard.insert(on, last);
                continue;
            };
            if let Some(last) = last {
                self.heard.insert(on.clone(), Some(last));
            }
            if !read {
                continue;
            }
            for saying in sayings.into_iter().filter(|saying| later(saying, seen)) {
                self.take(&on, saying, at);
            }
        }
    }

    fn take(&mut self, on: &Machine, saying: Saying, at: SystemTime) {
        let does = saying.say.and_then(|say| say.does);
        let waiting = |utterance: Utterance| {
            let Utterance { text } = utterance;
            Waiting { whose: Whose::Said { on: on.clone() }, text, came: Some(at) }
        };
        match does {
            Some(Does::InTurn(utterance)) => self.after(waiting(utterance)),
            Some(Does::Now(utterance)) => self.first(waiting(utterance)),
            Some(Does::Stop(())) => self.stop(),
            None => {},
        }
    }

    fn after(&mut self, mut waiting: Waiting) {
        if self.in_turn.back().is_some_and(|last| last.came.is_none()) {
            waiting.came = None;
        }
        self.in_turn.push_back(waiting);
        if self.in_turn.len() > WAITING {
            self.in_turn.pop_front();
        }
    }

    fn first(&mut self, waiting: Waiting) {
        self.at_once = Some(waiting);
        self.now = None;
    }

    fn stop(&mut self) {
        self.now = None;
        self.at_once = None;
        self.in_turn.clear();
        self.paused = false;
    }

    fn pause(&mut self, at: SystemTime) {
        if !self.paused {
            self.at_once.take_if(|waiting| waiting.stale(at));
            self.in_turn.retain(|waiting| !waiting.stale(at));
            self.paused = self.busy();
        }
    }

    fn unpause(&mut self) {
        if self.paused {
            self.paused = false;
            let waiting = self.at_once.iter_mut().chain(&mut self.in_turn);
            waiting.for_each(|waiting| waiting.came = None);
        }
    }

    fn waits(&self) -> bool {
        self.at_once.is_some() || !self.in_turn.is_empty()
    }

    fn busy(&self) -> bool {
        self.now.is_some() || self.waits()
    }

    fn next(&mut self, now: SystemTime) -> Option<Next> {
        loop {
            let next = self.at_once.take().or_else(|| self.in_turn.pop_front())?;
            if !self.paused && next.stale(now) {
                continue;
            }
            let Waiting { whose, text, .. } = next;
            return Some(Next { spoken: text.clone(), whose, text });
        }
    }

    fn ended(&mut self, number: u64) -> bool {
        let reading = self.now.as_ref().is_some_and(|now| now.number == number);
        if reading {
            self.now = None;
            self.paused = self.paused && self.waits();
        }
        reading
    }
}

fn later(saying: &Saying, seen: Option<(u32, u64)>) -> bool {
    seen.is_none_or(|(pid, number)| saying.pid != pid || saying.number > number)
}

fn drawn(c: char) -> bool {
    matches!(u32::from(c),
        0x2190..=0x21FF | 0x2300..=0x23FF
        | 0x2500..=0x25FF
        | 0x2600..=0x28FF
        | 0xE000..=0xF8FF | 0xF0000..=0xF_FFFD)
}

fn plain(selected: &str) -> String {
    let mut plain = String::new();
    for line in selected.lines() {
        let said: String = line.chars().filter(|c| !drawn(*c)).collect();
        for word in said.split_whitespace() {
            if !plain.is_empty() {
                plain.push(' ');
            }
            plain.push_str(word);
        }
        let boxed = line.contains(|c| matches!(u32::from(c), 0x2500..=0x259F));
        let ends = said.trim().is_empty() || boxed;
        if ends && !plain.is_empty() && !plain.ends_with(['.', '!', '?', ':', ';', ',']) {
            plain.push('.');
        }
    }
    plain
}

pub struct Aloud {
    queue: Queue,
    voice: Option<Voice>,
    fetching: Option<Fetching>,
    fetched: bool,
    dictating: bool,
    held: Arc<AtomicBool>,
    events: Sender<Event>,
}

struct Fetching {
    _asking: Asking,
    got: u64,
}

impl Aloud {
    pub fn new(events: Sender<Event>) -> Aloud {
        Aloud {
            queue: Queue::default(),
            voice: None,
            fetching: None,
            fetched: false,
            dictating: false,
            held: Arc::default(),
            events,
        }
    }

    pub fn hear(&mut self, machines: Vec<(Machine, Option<Vec<Saying>>)>, read: bool) {
        self.queue.hear(machines, read, SystemTime::now());
        self.go_on();
    }

    pub fn key(&mut self, selected: Option<&str>) -> Result<(), &'static str> {
        if self.queue.busy() {
            self.press(if self.queue.paused { Button::GoOn } else { Button::Pause });
            return Ok(());
        }
        let selected = selected.map(plain).filter(|text| !text.is_empty());
        let text = selected.ok_or("nothing to read aloud: select some text in a view first")?;
        let came = Some(SystemTime::now());
        self.queue.after(Waiting { whose: Whose::Selection, text, came });
        self.go_on();
        Ok(())
    }

    pub fn press(&mut self, button: Button) {
        match button {
            Button::Pause => self.queue.pause(SystemTime::now()),
            Button::GoOn => self.queue.unpause(),
            Button::Stop => self.queue.stop(),
        }
        self.keep_held();
    }

    pub fn hold(&mut self, dictating: bool) {
        self.dictating = dictating;
        self.keep_held();
    }

    fn keep_held(&self) {
        self.held.store(self.dictating || self.queue.paused, Ordering::Relaxed);
    }

    pub fn ended(&mut self, number: u64, ended: Result<(), String>) -> Option<String> {
        let reading = self.queue.ended(number);
        self.go_on();
        ended.err().filter(|_| reading)
    }

    pub fn fetch_went(&mut self, fetched: Fetched) -> Option<String> {
        match fetched {
            Fetched::Got(got) => {
                if let Some(fetching) = &mut self.fetching {
                    fetching.got = got;
                }
                None
            },
            Fetched::Ended(ended) => {
                self.fetching = None;
                match ended {
                    Ok(()) => {
                        self.fetched = true;
                        self.go_on();
                        None
                    },
                    Err(why) => {
                        self.press(Button::Stop);
                        Some(format!("the voice did not come: {why}"))
                    },
                }
            },
        }
    }

    fn go_on(&mut self) {
        self.read_next();
        self.keep_held();
    }

    fn read_next(&mut self) {
        if self.queue.now.is_some() || !self.queue.waits() {
            return;
        }
        self.fetched = self.fetched || voice::fetched();
        if !self.fetched {
            if self.fetching.is_none() {
                self.fetching = Some(Fetching { _asking: voice::fetch(&self.events), got: 0 });
            }
            return;
        }
        let Some(Next { spoken, whose, text }) = self.queue.next(SystemTime::now()) else {
            return;
        };
        let held = &self.held;
        let voice = self.voice.get_or_insert_with(|| Voice::start(Arc::clone(held)));
        self.queue.numbered += 1;
        let number = self.queue.numbered;
        let reading = voice.read(number, spoken, &self.events);
        let text = one_line(&text, SHOWN_CHARS);
        self.queue.now = Some(Now { number, whose, text, _reading: reading });
    }

    pub fn shown(&self) -> Option<Shown> {
        let paused = self.queue.paused;
        if let Some(Now { whose, text, .. }) = &self.queue.now {
            return Some(Shown::Reading { whose: whose.clone(), text: text.clone(), paused });
        }
        let fetching = self.fetching.as_ref().filter(|_| self.queue.waits());
        fetching.map(|fetching| Shown::Fetching { got: fetching.got, paused })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lunetad::api::Say;
    use lunetad::Destination;
    use ratatui::layout::Position;

    use super::*;
    use crate::sidebar::COLUMNS;
    use crate::speaker::Quiet;

    const PID: u32 = 7;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(1) + Duration::from_secs(seconds)
    }

    fn far() -> Machine {
        let far: Destination = "far".parse().unwrap();
        Machine::Remote(far)
    }

    fn saying(number: u64, does: Does) -> Saying {
        Saying { pid: PID, number, say: Some(Say { does: Some(does) }) }
    }

    fn utterance(text: &str) -> Utterance {
        Utterance { text: text.to_owned() }
    }

    fn in_turn(number: u64, text: &str) -> Saying {
        saying(number, Does::InTurn(utterance(text)))
    }

    fn here(sayings: Vec<Saying>) -> Vec<(Machine, Option<Vec<Saying>>)> {
        vec![(Machine::Local, Some(sayings))]
    }

    fn hearing() -> Queue {
        let mut queue = Queue::default();
        let quiet = vec![(Machine::Local, Some(Vec::new())), (far(), Some(Vec::new()))];
        queue.hear(quiet, true, at(0));
        queue
    }

    fn spoken(queue: &mut Queue) -> Vec<String> {
        std::iter::from_fn(|| queue.next(at(0))).map(|next| next.spoken).collect()
    }

    fn reading(queue: &mut Queue, number: u64) -> Quiet {
        let (reading, quiet) = Reading::new(Arc::default());
        let whose = Whose::Selection;
        queue.now = Some(Now { number, whose, text: String::new(), _reading: reading });
        quiet
    }

    #[test]
    fn what_a_machine_said_before_its_first_answer_says_nothing() {
        let mut queue = Queue::default();
        queue.hear(here(vec![in_turn(1, "before")]), true, at(0));
        assert!(!queue.busy());
        queue.hear(here(vec![in_turn(1, "before"), in_turn(2, "after")]), true, at(1));
        assert_eq!(spoken(&mut queue), ["after"]);
    }

    #[test]
    fn what_was_heard_is_read_once() {
        let mut queue = hearing();
        let one = in_turn(1, "one");
        queue.hear(here(vec![one.clone()]), true, at(1));
        queue.hear(here(vec![one.clone(), in_turn(2, "two")]), true, at(2));
        queue.hear(here(vec![one, in_turn(2, "two")]), true, at(3));
        assert_eq!(spoken(&mut queue), ["one", "two"]);
    }

    #[test]
    fn a_daemon_started_again_counts_afresh_and_is_heard() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(5, "old")]), true, at(1));
        let again = Saying { pid: PID + 1, ..in_turn(1, "new") };
        queue.hear(here(vec![again]), true, at(2));
        assert_eq!(spoken(&mut queue), ["old", "new"]);
    }

    #[test]
    fn a_machine_away_keeps_its_place_and_one_forgotten_begins_again() {
        let mut queue = hearing();
        let far_said = |sayings| vec![(Machine::Local, Some(Vec::new())), (far(), sayings)];
        queue.hear(far_said(None), true, at(1));
        queue.hear(far_said(Some(vec![in_turn(1, "while away")])), true, at(2));
        assert_eq!(spoken(&mut queue), ["while away"]);
        queue.hear(here(Vec::new()), true, at(3));
        queue.hear(far_said(Some(vec![in_turn(2, "gone a while")])), true, at(4));
        assert!(!queue.busy());
    }

    #[test]
    fn a_luneta_that_does_not_read_leaves_behind_what_it_heard() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "for the other")]), false, at(1));
        assert!(!queue.busy());
        let both = vec![in_turn(1, "for the other"), in_turn(2, "for this")];
        queue.hear(here(both), true, at(2));
        assert_eq!(spoken(&mut queue), ["for this"]);
    }

    #[test]
    fn past_the_most_that_wait_the_oldest_goes() {
        let mut queue = hearing();
        let many = (1..=WAITING as u64 + 2).map(|n| in_turn(n, &n.to_string())).collect();
        queue.hear(here(many), true, at(1));
        let read = spoken(&mut queue);
        assert_eq!(read.len(), WAITING);
        assert_eq!(read[0], "3");
    }

    #[test]
    fn now_goes_first_and_cuts_off_what_plays() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "next")]), true, at(1));
        let quiet = reading(&mut queue, 1);
        let now = saying(2, Does::Now(utterance("first")));
        queue.hear(here(vec![now]), true, at(2));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert_eq!(spoken(&mut queue), ["first", "next"]);
    }

    #[test]
    fn past_the_most_that_wait_what_goes_first_stays() {
        let mut queue = hearing();
        let mut said = vec![saying(1, Does::Now(utterance("first")))];
        said.extend((2..=WAITING as u64 + 2).map(|n| in_turn(n, &n.to_string())));
        queue.hear(here(said), true, at(1));
        let read = spoken(&mut queue);
        assert_eq!(read.len(), WAITING + 1);
        assert_eq!(read[0], "first");
        assert_eq!(read[1], "3");
    }

    #[test]
    fn what_goes_first_takes_the_place_of_one_that_has_not_begun() {
        let mut queue = hearing();
        let said = vec![
            in_turn(1, "next"),
            saying(2, Does::Now(utterance("cut off"))),
            saying(3, Does::Now(utterance("first"))),
        ];
        queue.hear(here(said), true, at(1));
        assert_eq!(spoken(&mut queue), ["first", "next"]);
    }

    #[test]
    fn stop_lets_go_of_what_was_to_go_first_too() {
        let mut queue = hearing();
        let said =
            vec![saying(1, Does::Now(utterance("first"))), saying(2, Does::Stop(()))];
        queue.hear(here(said), true, at(1));
        assert!(!queue.busy());
    }

    #[test]
    fn what_was_to_go_first_goes_once_it_has_waited_too_long() {
        let mut queue = hearing();
        queue.hear(here(vec![saying(1, Does::Now(utterance("early")))]), true, at(1));
        queue.hear(here(vec![in_turn(2, "late")]), true, at(1) + STALE);
        assert_eq!(queue.next(at(2) + STALE).map(|next| next.spoken).as_deref(), Some("late"));
    }

    #[test]
    fn stop_stops_and_clears_in_the_order_said() {
        let mut queue = hearing();
        let quiet = reading(&mut queue, 1);
        let said = vec![
            in_turn(1, "gone"),
            saying(2, Does::Stop(())),
            in_turn(3, "after the stop"),
        ];
        queue.hear(here(said), true, at(1));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert_eq!(spoken(&mut queue), ["after the stop"]);
    }

    #[test]
    fn what_has_waited_too_long_is_not_read() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early")]), true, at(1));
        queue.hear(here(vec![in_turn(2, "late")]), true, at(1) + STALE);
        let next = queue.next(at(2) + STALE).unwrap();
        assert_eq!(next.spoken, "late");
        assert_eq!(queue.next(at(2) + STALE), None);
    }

    #[test]
    fn only_what_is_being_read_ending_moves_on() {
        let mut queue = hearing();
        drop(reading(&mut queue, 2));
        assert!(!queue.ended(1), "stopped before it ended");
        assert!(queue.busy());
        assert!(queue.ended(2));
        assert!(!queue.busy());
    }

    fn aloud_reading() -> (Aloud, Quiet) {
        let mut aloud = Aloud::new(std::sync::mpsc::channel().0);
        let (reading, quiet) = Reading::new(Arc::clone(&aloud.held));
        let whose = Whose::Selection;
        aloud.queue.now = Some(Now { number: 1, whose, text: String::new(), _reading: reading });
        (aloud, quiet)
    }

    #[test]
    fn the_key_pauses_what_plays_and_has_it_go_on() {
        let (mut aloud, quiet) = aloud_reading();
        aloud.key(Some("selected")).expect("it pauses");
        assert!(quiet.held.load(Ordering::Relaxed));
        assert!(matches!(aloud.shown(), Some(Shown::Reading { paused: true, .. })));
        aloud.key(Some("selected")).expect("it goes on");
        assert!(!quiet.held.load(Ordering::Relaxed));
        assert!(matches!(aloud.shown(), Some(Shown::Reading { paused: false, .. })));
        assert!(!quiet.cut.load(Ordering::Relaxed));
        assert!(!aloud.queue.waits());
    }

    #[test]
    fn a_pause_and_dictating_each_hold_what_plays_until_both_are_over() {
        let (mut aloud, quiet) = aloud_reading();
        let held = || quiet.held.load(Ordering::Relaxed);
        aloud.press(Button::Pause);
        aloud.hold(true);
        aloud.hold(false);
        assert!(held(), "it is still paused");
        aloud.hold(true);
        aloud.press(Button::GoOn);
        assert!(held(), "you still dictate");
        aloud.hold(false);
        assert!(!held());
    }

    #[test]
    fn a_pause_ends_with_what_it_paused() {
        let ended: [fn(&mut Aloud); 3] = [
            |aloud| aloud.press(Button::Stop),
            |aloud| drop(aloud.ended(1, Ok(()))),
            |aloud| {
                aloud.hear(here(Vec::new()), true);
                aloud.hear(here(vec![saying(1, Does::Stop(()))]), true);
            },
        ];
        for ends in ended {
            let (mut aloud, quiet) = aloud_reading();
            aloud.press(Button::Pause);
            ends(&mut aloud);
            assert!(!quiet.held.load(Ordering::Relaxed));
            assert_eq!(aloud.shown(), None);
            assert!(!aloud.queue.busy());
            assert!(!aloud.queue.paused);
        }
    }

    #[test]
    fn a_button_with_nothing_left_to_do_does_nothing() {
        let mut aloud = Aloud::new(std::sync::mpsc::channel().0);
        for button in [Button::Pause, Button::GoOn] {
            aloud.press(button);
            assert!(!aloud.queue.paused);
            assert!(!aloud.queue.busy());
        }
    }

    #[test]
    fn a_button_drawn_before_a_change_does_what_it_shows_or_nothing() {
        let (mut aloud, quiet) = aloud_reading();
        aloud.press(Button::GoOn);
        assert!(!quiet.held.load(Ordering::Relaxed), "what plays plays on");
        aloud.press(Button::Pause);
        aloud.press(Button::Pause);
        assert!(quiet.held.load(Ordering::Relaxed), "what is paused stays paused");
    }

    #[test]
    fn what_is_said_after_a_stop_is_not_paused() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.pause(at(1));
        let said = vec![saying(1, Does::Stop(())), in_turn(2, "after the stop")];
        queue.hear(here(said), true, at(2));
        assert!(!queue.paused);
        assert_eq!(spoken(&mut queue), ["after the stop"]);
    }

    #[test]
    fn what_goes_first_does_not_end_a_pause() {
        let mut queue = hearing();
        let quiet = reading(&mut queue, 1);
        queue.pause(at(1));
        queue.hear(here(vec![saying(1, Does::Now(utterance("first")))]), true, at(2));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert!(queue.paused);
        assert_eq!(spoken(&mut queue), ["first"]);
    }

    #[test]
    fn what_waited_through_a_pause_is_all_read_however_long_it_takes() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.pause(at(1));
        queue.hear(here(vec![saying(1, Does::Now(utterance("first")))]), true, at(1));
        let last = WAITING as u64 + 1;
        let rest = (2..=last).map(|n| in_turn(n, &n.to_string())).collect();
        queue.hear(here(rest), true, at(2));
        queue.unpause();
        let mut clock = at(3) + STALE * 5;
        let read: Vec<String> = std::iter::from_fn(|| {
            clock += STALE * 2;
            queue.next(clock)
        })
        .map(|next| next.spoken)
        .collect();
        assert_eq!(read.len(), WAITING + 1);
        assert_eq!(read[0], "first");
        assert_eq!(read[WAITING], last.to_string());
    }

    #[test]
    fn what_comes_behind_what_waited_through_a_pause_is_read_too() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "waited")]), true, at(1));
        queue.pause(at(1));
        queue.unpause();
        queue.hear(here(vec![in_turn(2, "behind")]), true, at(2));
        let late = at(2) + STALE * 5;
        assert_eq!(queue.next(late).map(|next| next.spoken).as_deref(), Some("waited"));
        assert_eq!(queue.next(late).map(|next| next.spoken).as_deref(), Some("behind"));
        queue.hear(here(vec![in_turn(3, "after")]), true, at(3));
        assert_eq!(queue.next(late), None, "it waited on nothing of yours");
    }

    #[test]
    fn a_pause_does_not_bring_back_what_had_waited_too_long() {
        let mut queue = hearing();
        let early = vec![saying(1, Does::Now(utterance("first"))), in_turn(2, "early")];
        queue.hear(here(early), true, at(1));
        queue.hear(here(vec![in_turn(3, "late")]), true, at(1) + STALE);
        drop(reading(&mut queue, 1));
        queue.pause(at(2) + STALE);
        queue.unpause();
        let later = at(3) + STALE * 5;
        assert_eq!(queue.next(later).map(|next| next.spoken).as_deref(), Some("late"));
        assert_eq!(queue.next(later), None);
    }

    #[test]
    fn a_pause_of_only_what_had_waited_too_long_is_none() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early")]), true, at(1));
        queue.pause(at(2) + STALE);
        assert!(!queue.paused);
        assert!(!queue.busy());
    }

    #[test]
    fn pausing_what_is_paused_lets_go_of_nothing() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "rest")]), true, at(1));
        queue.pause(at(1));
        queue.pause(at(2) + STALE * 5);
        assert!(queue.paused);
        assert_eq!(queue.in_turn.len(), 1);
    }

    #[test]
    fn nothing_goes_stale_during_a_pause() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "rest")]), true, at(1));
        queue.pause(at(1));
        assert!(queue.ended(1));
        assert!(queue.paused, "what waits is what you paused too");
        assert_eq!(queue.next(at(2) + STALE * 5).map(|next| next.spoken).as_deref(), Some("rest"));
    }

    #[test]
    fn going_on_with_nothing_paused_gives_what_waits_no_more_time() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early")]), true, at(1));
        queue.unpause();
        assert_eq!(queue.next(at(2) + STALE), None);
    }

    #[test]
    fn a_click_hits_the_icon_it_is_on_at_any_width_and_of_any_text() {
        let texts = [
            "the tests pass",
            "\u{1b}[31mFAIL\u{1b}[0m",
            "a\u{7}b\u{7}c",
            "\u{1b}\u{1b}\u{1b}x",
            "\u{85}\u{9b}x",
            "日本語のテキストです",
            "لا لا لا لا",
            "",
        ];
        for text in texts {
            for width in 0..=48 {
                for (paused, toggle) in [(false, Button::Pause), (true, Button::GoOn)] {
                    let (whose, text) = (Whose::Selection, one_line(text, SHOWN_CHARS));
                    let (line, spots) = Shown::Reading { whose, text, paused }.line(width.into());
                    let area = Rect::new(0, 0, width, 1);
                    let mut buffer = Buffer::empty(area);
                    buffer.set_line(0, 0, &line, width);
                    let spots = spots.placed(area);
                    for button in [toggle, Button::Stop] {
                        let icon = (0..width).find(|x| buffer[(*x, 0)].symbol() == button.icon());
                        let hit = |x| spots.at(Position::new(x, 0)) == Some(&button);
                        let spot = (0..width).any(hit);
                        assert_eq!(icon.is_some(), spot, "{button:?} in {width} of {line:?}");
                        if let Some(icon) = icon {
                            assert!(hit(icon), "{button:?} at {icon} in {width} of {line:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn what_is_not_drawn_does_not_push_the_buttons_along() {
        for text in ["\u{1b}[31mFAIL\u{1b}[0m", "a\u{7}b\u{7}c"] {
            let (whose, text) = (Whose::Selection, text.to_owned());
            let (line, spots) = Shown::Reading { whose, text, paused: false }.line(31);
            let area = Rect::new(0, 0, 31, 1);
            let mut buffer = Buffer::empty(area);
            buffer.set_line(0, 0, &line, 31);
            assert_eq!(buffer[(29, 0)].symbol(), STOP);
            assert_eq!(spots.placed(area).at(Position::new(29, 0)), Some(&Button::Stop));
        }
    }

    #[test]
    fn the_fetch_says_how_far_it_got_at_the_width_the_sidebar_starts_at() {
        let shown = Shown::Fetching { got: 12_000_000, paused: false };
        let (line, _) = shown.line(usize::from(COLUMNS) - 1);
        let said: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        let far = format!("12 of {} MB", voice::BYTES / 1_000_000);
        assert!(said.contains(&far), "{said}");
    }

    #[test]
    fn alt_shift_u_stops_as_either_kind_of_terminal_says_it() {
        let both = KeyModifiers::ALT | KeyModifiers::SHIFT;
        assert!(stops((KeyCode::Char('u'), both)));
        assert!(stops((KeyCode::Char('U'), both)));
        assert!(!stops(KEY));
        assert!(!stops((KeyCode::Char('U'), KeyModifiers::SHIFT)));
        assert!(!stops((KeyCode::Char('i'), both)));
    }

    #[test]
    fn a_view_s_frames_and_marks_are_not_read() {
        let selected = "⏺ The fix is in. The daemon keeps what\n  was said.\n\n  ⎿  ok\n";
        assert_eq!(plain(selected), "The fix is in. The daemon keeps what was said. ok");
        assert_eq!(plain("✳ Thinking… (esc to interrupt)"), "Thinking… (esc to interrupt)");
        assert_eq!(plain("\u{f057e} ● a"), "a");
        assert_eq!(plain("──────\n│ │\n"), "");
    }

    #[test]
    fn a_paragraph_or_a_row_of_a_box_ends_a_sentence() {
        assert_eq!(plain("Summary\n\nIt works"), "Summary. It works");
        assert_eq!(plain("Asked:\n\nIt works"), "Asked: It works");
        let table = "┌──────┬─────┐\n│ name │ age │\n├──────┼─────┤\n│ a    │ 2   │\n└─────┘";
        assert_eq!(plain(table), "name age. a 2.");
    }

    #[test]
    fn what_is_being_read_has_a_line_of_whose_it_is_and_what_it_says() {
        let line = |whose| {
            let text = "the tests pass".to_owned();
            let line = Shown::Reading { whose, text, paused: false }.said(40);
            line.spans.iter().map(|span| span.content.as_ref()).collect::<String>()
        };
        assert_eq!(line(Whose::Selection), format!("{MARK} selection · the tests pass"));
        assert_eq!(line(Whose::Said { on: far() }), format!("{MARK} far · the tests pass"));
    }
}
