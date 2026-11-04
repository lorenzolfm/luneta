//! What luneta reads aloud, on the machine you sit at: what `luneta say` gave
//! the daemon of any machine it watches, as the host says it, and what you
//! selected, by `Alt u`. While anything plays, `Alt u` pauses it and has it go
//! on, and `Alt Shift u` stops it, and lets go of what waits. What plays waits
//! while you dictate too, or the microphone would write the agent's words into
//! your prompt.
//!
//! One luneta of a machine reads, the one that puts its notifications up, as
//! two would talk over each other; and those keys in another have it do so. A
//! luneta over ssh reads nothing: its speakers are not where you sit. What came
//! before a machine's first answer says nothing, as it does with notifications.
//!
//! What waits is read in the order it came, with the oldest let go of past
//! `WAITING`, and what has waited past `STALE`, naming the session as the
//! speaker changes. What `--now` gave goes before all of it, and only the last
//! of those, as it would have cut off the one before. A pause is yours, and
//! holds what `--now` gave too: what waited through it no longer goes for its
//! age, and nor does what comes behind that, with `WAITING` the limit of them.
//! The sidebar has a line of what is being read, with a button for each of those
//! keys, and a mark on its agent's row: no box, as a box would take the keys. The
//! sidebar collapsed has no room for the line, and shows only a pause, as what
//! plays you can hear.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyModifiers};
use lunetad::api::say::Does;
use lunetad::api::{Saying, Utterance, Zellij};
use lunetad::sayings::STALE;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::brief::{one_line, Asking};
use crate::host::Event;
use crate::machines::{self, Machine};
use crate::models::Fetched;
use crate::sidebar::{cut, ACCENT, NAME};
use crate::spots::Spots;
use crate::voice::{self, Reading, Voice};

/// The key that reads what you selected, and pauses what plays or has it go on.
pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('u'), KeyModifiers::ALT);

/// Whether `chord` is `Alt Shift u`, which stops what plays and lets go of what
/// waits: `u` as kitty's keys say it, and `U` as a terminal without them types
/// it.
pub fn stops((code, modifiers): (KeyCode, KeyModifiers)) -> bool {
    matches!(code, KeyCode::Char('u' | 'U')) && modifiers == KeyModifiers::ALT | KeyModifiers::SHIFT
}

/// The mark of what is being read, `md-volume_high` of the Nerd Fonts.
pub const MARK: &str = "\u{f057e}";

/// The button that pauses what plays, `md-pause` of the Nerd Fonts.
pub const PAUSE: &str = "\u{f03e4}";

/// The button that has what is paused go on, `md-play`.
pub const PLAY: &str = "\u{f040a}";

/// The button that stops what plays, `md-stop`.
pub const STOP: &str = "\u{f04db}";

/// How wide a button is, of what a click hits: its icon, and a column each side
/// of it, as an icon alone is hard to hit.
const BUTTON: usize = 3;

/// A button on the sidebar's line of what is being read. Each does what it
/// shows, or nothing: one drawn before things changed never does the opposite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Pauses what plays.
    Pause,
    /// Has what is paused go on.
    GoOn,
    /// Stops what plays, and lets go of what waits.
    Stop,
}

impl Button {
    /// What it shows.
    fn icon(self) -> &'static str {
        match self {
            Button::Pause => PAUSE,
            Button::GoOn => PLAY,
            Button::Stop => STOP,
        }
    }
}

/// The most that wait to be read: an agent's handoff is a few, and each of five
/// agents at work has one or two.
const WAITING: usize = 16;

/// How much of what is being read the sidebar holds on to, more than its line
/// has room for.
const SHOWN_CHARS: usize = 200;

/// Whose something to read is.
#[derive(Clone, Debug, PartialEq)]
pub enum Whose {
    /// Yours, by `Alt u`.
    Selection,
    /// `luneta say`'s on a machine, from a pane there, if it ran in one.
    Said { on: Machine, speaker: Option<Zellij> },
}

/// What a session is to be called, of the pane `speaker`: what its daemon says
/// it is now, or else what the pane knows it by.
fn called(speaker: &Zellij) -> &str {
    speaker.session.as_deref().unwrap_or(&speaker.named)
}

/// What the sidebar shows of what goes on.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// What is being read, on one line, and whether you paused it.
    Reading { whose: Whose, text: String, paused: bool },
    /// What waits waits for the model, of which this many bytes are in, and
    /// whether you paused it.
    Fetching { got: u64, paused: bool },
}

impl Shown {
    /// The pane this comes from, for the mark on its row: its machine, the
    /// session it is in, where its daemon could tell, and its id.
    pub fn from(&self) -> Option<(&Machine, &str, u32)> {
        let Shown::Reading { whose: Whose::Said { on, speaker: Some(speaker) }, .. } = self else {
            return None;
        };
        Some((on, speaker.session.as_deref()?, speaker.pane))
    }

    /// Whether you paused it.
    pub fn paused(&self) -> bool {
        match self {
            Shown::Reading { paused, .. } | Shown::Fetching { paused, .. } => *paused,
        }
    }

    /// The sidebar's line of it, `width` wide, its buttons at its right, and
    /// where they are on it. They go by what is drawn, whatever it says, and
    /// a button whose icon does not fit has no spot, so that a click hits what
    /// it is on, or nothing.
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

    /// What it says on that line, in `width`.
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
                    Whose::Said { speaker: Some(speaker), .. } => called(speaker).to_owned(),
                    Whose::Said { on: Machine::Local, speaker: None } => machines::local_name(),
                    Whose::Said { on: Machine::Remote(on), speaker: None } => on.to_string(),
                };
                let who = cut(&who, room);
                let rest = room.saturating_sub(who.width() + 3);
                let name = Span::styled(who, Style::new().fg(NAME));
                let mut said = vec![mark, Span::raw(" "), name];
                // Short of room for any of it, it would push the buttons along.
                if rest > 0 {
                    said.push(Span::styled(format!(" · {}", cut(text, rest)), dim));
                }
                Line::from(said)
            },
        }
    }
}

/// How many columns `line` takes when drawn, up to `width`: `Line::width` counts
/// the control characters an agent's words can hold, which are not drawn.
fn drawn_width(line: &Line<'_>, width: usize) -> usize {
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    usize::from(buffer.set_line(0, 0, line, width).0)
}

/// What waits to be read.
#[derive(Debug)]
struct Waiting {
    whose: Whose,
    text: String,
    /// When it came, what its age is told from: none once it has waited on you,
    /// through a pause or behind what did, as its age is then your doing.
    came: Option<SystemTime>,
}

impl Waiting {
    /// Whether it has waited too long, at `now`.
    fn stale(&self, now: SystemTime) -> bool {
        self.came.is_some_and(|came| now.duration_since(came).is_ok_and(|age| age > STALE))
    }
}

/// What is being read.
#[derive(Debug)]
struct Now {
    number: u64,
    whose: Whose,
    /// On one line, for the sidebar.
    text: String,
    /// Dropped, it stops.
    _reading: Reading,
}

/// What is to be read next: as it is to be said, and as it came.
#[derive(Debug, PartialEq)]
struct Next {
    spoken: String,
    whose: Whose,
    text: String,
}

/// What is being read, what waits, and what was heard of each machine, with
/// no thread, sound or model in it.
#[derive(Debug, Default)]
struct Queue {
    /// What goes before all that waits, by `--now`.
    at_once: Option<Waiting>,
    /// What waits its turn, oldest first, as all that is taken goes in at the
    /// back: what is let go of from the front is the oldest.
    in_turn: VecDeque<Waiting>,
    now: Option<Now>,
    /// The number of the last read.
    numbered: u64,
    /// The session whose name was said last, of the last that named one: none
    /// after what came from no session.
    named: Option<(Machine, String)>,
    /// The last heard of each machine, by the daemon that numbered it: a
    /// machine here has answered, with nothing said yet if none.
    heard: HashMap<Machine, Option<(u32, u64)>>,
    /// Whether you paused what is being read, and what waits: until you have
    /// them go on, or nothing is left of them.
    paused: bool,
}

impl Queue {
    /// Takes in what each machine lunetad watches last said, as `machines` has
    /// it, as of `at`: none of one it does not reach now, which keeps where it
    /// was. A machine left out, which it no longer watches, begins again. What
    /// a machine says in its first answer came before luneta heard it, and
    /// goes unread, and so does all of it where this luneta does not `read`,
    /// which it leaves behind, and reads on from once it does.
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

    /// Takes in what `on` said at `at`. One that does nothing luneta knows
    /// comes of a newer daemon.
    fn take(&mut self, on: &Machine, saying: Saying, at: SystemTime) {
        let does = saying.say.and_then(|say| say.does);
        let waiting = |utterance: Utterance| {
            let whose = Whose::Said { on: on.clone(), speaker: utterance.speaker };
            Waiting { whose, text: utterance.text, came: Some(at) }
        };
        match does {
            Some(Does::InTurn(utterance)) => self.after(waiting(utterance)),
            Some(Does::Now(utterance)) => self.first(waiting(utterance)),
            Some(Does::Stop(_)) => self.stop(),
            None => {},
        }
    }

    /// `waiting` waits behind the rest, and the oldest goes where too many do.
    /// Behind what waited on you, it waits on you too: it would have been read
    /// by now but for the pause.
    fn after(&mut self, mut waiting: Waiting) {
        if self.in_turn.back().is_some_and(|last| last.came.is_none()) {
            waiting.came = None;
        }
        self.in_turn.push_back(waiting);
        if self.in_turn.len() > WAITING {
            self.in_turn.pop_front();
        }
    }

    /// `waiting` is next, in place of one that had not begun, which it would
    /// have cut off, and what is being read stops.
    fn first(&mut self, waiting: Waiting) {
        self.at_once = Some(waiting);
        self.now = None;
    }

    /// What is being read stops, and what waits goes, and with them any pause
    /// of them: what is said after this plays.
    fn stop(&mut self) {
        self.now = None;
        self.at_once = None;
        self.in_turn.clear();
        self.paused = false;
    }

    /// Pauses what is being read and what waits, where there is any, as of `at`:
    /// a pause of nothing would keep what is said next silent, with no line to
    /// say why. What had waited too long by then goes, as it would have with no
    /// pause, or going on would bring it back.
    fn pause(&mut self, at: SystemTime) {
        if !self.paused {
            self.at_once.take_if(|waiting| waiting.stale(at));
            self.in_turn.retain(|waiting| !waiting.stale(at));
            self.paused = self.busy();
        }
    }

    /// Has what you paused go on. You had what waits wait, so its age no longer
    /// counts, however long it takes to read: one deadline for all of it would
    /// lose the last of it, an agent's handoff, where it takes over `STALE`. What
    /// you did not pause did not wait on you.
    fn unpause(&mut self) {
        if self.paused {
            self.paused = false;
            let waiting = self.at_once.iter_mut().chain(&mut self.in_turn);
            waiting.for_each(|waiting| waiting.came = None);
        }
    }

    /// Whether anything waits to be read.
    fn waits(&self) -> bool {
        self.at_once.is_some() || !self.in_turn.is_empty()
    }

    /// Whether anything is being read, or waits to be.
    fn busy(&self) -> bool {
        self.now.is_some() || self.waits()
    }

    /// What to read next, at `now`, of what has not waited too long, led by
    /// the name of its session where the speaker changed: the name is all
    /// there is to tell a voice from another by. Nothing waits too long during
    /// a pause, after which `unpause` has its age no longer count.
    fn next(&mut self, now: SystemTime) -> Option<Next> {
        loop {
            let next = self.at_once.take().or_else(|| self.in_turn.pop_front())?;
            if !self.paused && next.stale(now) {
                continue;
            }
            let Waiting { whose, text, .. } = next;
            let speaker = match &whose {
                Whose::Said { on, speaker: Some(speaker) } => {
                    Some((on.clone(), called(speaker).to_owned()))
                },
                Whose::Said { speaker: None, .. } | Whose::Selection => None,
            };
            let spoken = match &speaker {
                Some((_, session)) if self.named != speaker => format!("{session}. {text}"),
                Some(_) | None => text.clone(),
            };
            self.named = speaker;
            return Some(Next { spoken, whose, text });
        }
    }

    /// The reading numbered `number` ended: whether that is what is being read,
    /// and so what waits is next. What was stopped before it ended has said
    /// all it had to. A pause ends with the last of what it held: what comes
    /// after that was not what you paused.
    fn ended(&mut self, number: u64) -> bool {
        let reading = self.now.as_ref().is_some_and(|now| now.number == number);
        if reading {
            self.now = None;
            self.paused = self.paused && self.waits();
        }
        reading
    }
}

/// Whether `saying` came after `seen`: another daemon's, which counts afresh,
/// came after what the one before it said.
fn later(saying: &Saying, seen: Option<(u32, u64)>) -> bool {
    seen.is_none_or(|(pid, number)| saying.pid != pid || saying.number > number)
}

/// Whether `c` is drawn, and not said: what a terminal program draws its frames
/// and marks with, as Claude Code does, which `ptts` would take for words, and
/// a font's icons. Letters, digits and punctuation are not.
fn drawn(c: char) -> bool {
    matches!(u32::from(c),
        // Arrows, and symbols of math and of technical drawing, as `⏺` and `⎿`.
        0x2190..=0x21FF | 0x2300..=0x23FF
        // Box drawing, blocks and shapes.
        | 0x2500..=0x25FF
        // Symbols, dingbats, as `✳`, and braille, as a spinner's.
        | 0x2600..=0x28FF
        // The Nerd Fonts' icons.
        | 0xE000..=0xF8FF | 0xF0000..=0xF_FFFD)
}

/// What a pane drew, as a selection has it, as it is to be read: without what
/// frames and marks it, each row of a box a sentence, and the end of a
/// paragraph the end of a sentence. `ptts` joins the lines a pane breaks at its
/// width, and takes it from there, numbers and symbols and all.
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

/// What luneta reads aloud, from the first thing it has to read until it ends.
pub struct Aloud {
    queue: Queue,
    /// The thread that runs the voice, from the first thing it reads on.
    voice: Option<Voice>,
    /// The fetch of the model, while it is on its way.
    fetching: Option<Fetching>,
    /// Whether the model is in, once luneta has found that it is.
    fetched: bool,
    /// Whether you dictate.
    dictating: bool,
    /// Has what plays wait, while you dictate, or have it paused: the end of
    /// either leaves the other as it is.
    held: Arc<AtomicBool>,
    events: Sender<Event>,
}

/// The fetch of the model, and how many bytes of it are in.
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

    /// Takes in what each machine said, as `Queue::hear` does, and reads what
    /// is next.
    pub fn hear(&mut self, machines: Vec<(Machine, Option<Vec<Saying>>)>, read: bool) {
        self.queue.hear(machines, read, SystemTime::now());
        self.go_on();
    }

    /// `Alt u`: pauses what plays or has it go on, where anything does, and
    /// else reads `selected`. Or why it reads nothing.
    pub fn key(&mut self, selected: Option<&str>) -> Result<(), &'static str> {
        if self.queue.busy() {
            self.press(if self.queue.paused { Button::GoOn } else { Button::Pause });
            return Ok(());
        }
        let selected = selected.map(plain).filter(|text| !text.is_empty());
        let text = selected.ok_or("nothing to read aloud: select some text in a pane first")?;
        let came = Some(SystemTime::now());
        self.queue.after(Waiting { whose: Whose::Selection, text, came });
        self.go_on();
        Ok(())
    }

    /// Does what `button` does. One drawn before what it was for ended does
    /// nothing, as what you selected is not what you clicked it for.
    pub fn press(&mut self, button: Button) {
        match button {
            Button::Pause => self.queue.pause(SystemTime::now()),
            Button::GoOn => self.queue.unpause(),
            Button::Stop => self.queue.stop(),
        }
        self.keep_held();
    }

    /// Has what plays wait while you dictate, as `dictating` says, and go on
    /// after, unless you paused it.
    pub fn hold(&mut self, dictating: bool) {
        self.dictating = dictating;
        self.keep_held();
    }

    /// Has what plays wait for as long as anything has it wait.
    fn keep_held(&self) {
        self.held.store(self.dictating || self.queue.paused, Ordering::Relaxed);
    }

    /// The reading numbered `number` ended as `ended` says, and what waits is
    /// next: or why it went wrong, while it was what was being read.
    pub fn ended(&mut self, number: u64, ended: Result<(), String>) -> Option<String> {
        let reading = self.queue.ended(number);
        self.go_on();
        ended.err().filter(|_| reading)
    }

    /// Takes in how the fetch of the model goes: once it is in, what waits is
    /// next, and where it did not come, what waits goes, and why not.
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

    /// Reads what is next, where nothing is being read. What comes next of what
    /// you paused starts paused.
    fn go_on(&mut self) {
        self.read_next();
        self.keep_held();
    }

    /// Reads what is next, where nothing is being read, once the model is in,
    /// which what waits has fetched.
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

    /// What the sidebar shows of it.
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

    use lunetad::api::say::Stop;
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

    fn pane(named: &str, session: Option<&str>) -> Zellij {
        Zellij { named: named.to_owned(), session: session.map(str::to_owned), pane: 3 }
    }

    fn utterance(text: &str, session: Option<&str>) -> Utterance {
        let speaker = session.map(|session| pane(session, Some(session)));
        Utterance { text: text.to_owned(), speaker }
    }

    fn in_turn(number: u64, text: &str, session: Option<&str>) -> Saying {
        saying(number, Does::InTurn(utterance(text, session)))
    }

    fn here(sayings: Vec<Saying>) -> Vec<(Machine, Option<Vec<Saying>>)> {
        vec![(Machine::Local, Some(sayings))]
    }

    /// A queue that has heard from this machine, and from `far` where named.
    fn hearing() -> Queue {
        let mut queue = Queue::default();
        let quiet = vec![(Machine::Local, Some(Vec::new())), (far(), Some(Vec::new()))];
        queue.hear(quiet, true, at(0));
        queue
    }

    fn spoken(queue: &mut Queue) -> Vec<String> {
        std::iter::from_fn(|| queue.next(at(0))).map(|next| next.spoken).collect()
    }

    /// Something being read, which says when it was cut off.
    fn reading(queue: &mut Queue, number: u64) -> Quiet {
        let (reading, quiet) = Reading::new(Arc::default());
        let whose = Whose::Selection;
        queue.now = Some(Now { number, whose, text: String::new(), _reading: reading });
        quiet
    }

    /// A window that opens does not read out what was said before it.
    #[test]
    fn what_a_machine_said_before_its_first_answer_says_nothing() {
        let mut queue = Queue::default();
        queue.hear(here(vec![in_turn(1, "before", None)]), true, at(0));
        assert!(!queue.busy());
        queue.hear(here(vec![in_turn(1, "before", None), in_turn(2, "after", None)]), true, at(1));
        assert_eq!(spoken(&mut queue), ["after"]);
    }

    /// Each answer lists what was said lately, and what is read is read once.
    #[test]
    fn what_was_heard_is_read_once() {
        let mut queue = hearing();
        let one = in_turn(1, "one", None);
        queue.hear(here(vec![one.clone()]), true, at(1));
        queue.hear(here(vec![one.clone(), in_turn(2, "two", None)]), true, at(2));
        queue.hear(here(vec![one, in_turn(2, "two", None)]), true, at(3));
        assert_eq!(spoken(&mut queue), ["one", "two"]);
    }

    #[test]
    fn a_daemon_started_again_counts_afresh_and_is_heard() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(5, "old", None)]), true, at(1));
        let again = Saying { pid: PID + 1, ..in_turn(1, "new", None) };
        queue.hear(here(vec![again]), true, at(2));
        assert_eq!(spoken(&mut queue), ["old", "new"]);
    }

    /// A machine out of reach keeps its place, and one forgotten does not:
    /// what it said while it was forgotten came before its next first answer.
    #[test]
    fn a_machine_away_keeps_its_place_and_one_forgotten_begins_again() {
        let mut queue = hearing();
        let far_said = |sayings| vec![(Machine::Local, Some(Vec::new())), (far(), sayings)];
        queue.hear(far_said(None), true, at(1));
        queue.hear(far_said(Some(vec![in_turn(1, "while away", None)])), true, at(2));
        assert_eq!(spoken(&mut queue), ["while away"]);
        queue.hear(here(Vec::new()), true, at(3));
        queue.hear(far_said(Some(vec![in_turn(2, "gone a while", None)])), true, at(4));
        assert!(!queue.busy());
    }

    /// The window that does not hold the lock keeps up, and on taking the lock
    /// reads on from there.
    #[test]
    fn a_luneta_that_does_not_read_leaves_behind_what_it_heard() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "for the other", None)]), false, at(1));
        assert!(!queue.busy());
        let both = vec![in_turn(1, "for the other", None), in_turn(2, "for this", None)];
        queue.hear(here(both), true, at(2));
        assert_eq!(spoken(&mut queue), ["for this"]);
    }

    #[test]
    fn past_the_most_that_wait_the_oldest_goes() {
        let mut queue = hearing();
        let many = (1..=WAITING as u64 + 2).map(|n| in_turn(n, &n.to_string(), None)).collect();
        queue.hear(here(many), true, at(1));
        let read = spoken(&mut queue);
        assert_eq!(read.len(), WAITING);
        assert_eq!(read[0], "3");
    }

    #[test]
    fn now_goes_first_and_cuts_off_what_plays() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "next", None)]), true, at(1));
        let quiet = reading(&mut queue, 1);
        let now = saying(2, Does::Now(utterance("first", None)));
        queue.hear(here(vec![now]), true, at(2));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert_eq!(spoken(&mut queue), ["first", "next"]);
    }

    /// What is to go first is no one's oldest, however many come behind it.
    #[test]
    fn past_the_most_that_wait_what_goes_first_stays() {
        let mut queue = hearing();
        let mut said = vec![saying(1, Does::Now(utterance("first", None)))];
        said.extend((2..=WAITING as u64 + 2).map(|n| in_turn(n, &n.to_string(), None)));
        queue.hear(here(said), true, at(1));
        let read = spoken(&mut queue);
        assert_eq!(read.len(), WAITING + 1);
        assert_eq!(read[0], "first");
        assert_eq!(read[1], "3");
    }

    /// Two in one answer end as they do in two: the second would have cut off
    /// the first.
    #[test]
    fn what_goes_first_takes_the_place_of_one_that_has_not_begun() {
        let mut queue = hearing();
        let said = vec![
            in_turn(1, "next", None),
            saying(2, Does::Now(utterance("cut off", None))),
            saying(3, Does::Now(utterance("first", None))),
        ];
        queue.hear(here(said), true, at(1));
        assert_eq!(spoken(&mut queue), ["first", "next"]);
    }

    #[test]
    fn stop_lets_go_of_what_was_to_go_first_too() {
        let mut queue = hearing();
        let said =
            vec![saying(1, Does::Now(utterance("first", None))), saying(2, Does::Stop(Stop {}))];
        queue.hear(here(said), true, at(1));
        assert!(!queue.busy());
    }

    /// What was to go first has waited too long as all else has, and what is
    /// behind it is next.
    #[test]
    fn what_was_to_go_first_goes_once_it_has_waited_too_long() {
        let mut queue = hearing();
        queue.hear(here(vec![saying(1, Does::Now(utterance("early", None)))]), true, at(1));
        queue.hear(here(vec![in_turn(2, "late", None)]), true, at(1) + STALE);
        assert_eq!(queue.next(at(2) + STALE).map(|next| next.spoken).as_deref(), Some("late"));
    }

    #[test]
    fn stop_stops_and_clears_in_the_order_said() {
        let mut queue = hearing();
        let quiet = reading(&mut queue, 1);
        let said = vec![
            in_turn(1, "gone", None),
            saying(2, Does::Stop(Stop {})),
            in_turn(3, "after the stop", None),
        ];
        queue.hear(here(said), true, at(1));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert_eq!(spoken(&mut queue), ["after the stop"]);
    }

    #[test]
    fn the_session_is_named_as_the_speaker_changes() {
        let mut queue = hearing();
        let said = vec![
            in_turn(1, "one", Some("luneta")),
            in_turn(2, "two", Some("luneta")),
            in_turn(3, "three", Some("wt")),
            in_turn(4, "four", Some("luneta")),
            in_turn(5, "no pane", None),
            in_turn(6, "six", Some("luneta")),
        ];
        queue.hear(here(said), true, at(1));
        assert_eq!(
            spoken(&mut queue),
            ["luneta. one", "two", "wt. three", "luneta. four", "no pane", "luneta. six"],
        );
    }

    /// One name on two machines is two speakers.
    #[test]
    fn a_session_of_that_name_on_another_machine_is_another_speaker() {
        let mut queue = hearing();
        let said = |text, number| Some(vec![in_turn(number, text, Some("main"))]);
        let here_said = said("here", 1);
        let far_quiet = (far(), Some(Vec::new()));
        queue.hear(vec![(Machine::Local, here_said.clone()), far_quiet], true, at(1));
        queue.hear(vec![(Machine::Local, here_said), (far(), said("there", 1))], true, at(1));
        assert_eq!(spoken(&mut queue), ["main. here", "main. there"]);
    }

    /// A Mac that slept does not read out what it had waiting when it did.
    #[test]
    fn what_has_waited_too_long_is_not_read() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early", None)]), true, at(1));
        queue.hear(here(vec![in_turn(2, "late", None)]), true, at(1) + STALE);
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

    /// `Aloud` with something being read, and with no voice to read what comes
    /// next: and what has it keep quiet.
    fn aloud_reading() -> (Aloud, Quiet) {
        let mut aloud = Aloud::new(std::sync::mpsc::channel().0);
        let (reading, quiet) = Reading::new(Arc::clone(&aloud.held));
        let whose = Whose::Selection;
        aloud.queue.now = Some(Now { number: 1, whose, text: String::new(), _reading: reading });
        (aloud, quiet)
    }

    /// `Alt u` pauses what plays and has it go on, and does not read what is
    /// selected meanwhile.
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

    /// Each has what plays wait until it is over, whichever ends first: closing
    /// the dictation box does not start what you paused, and going on while you
    /// dictate does not play into the microphone.
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

    /// Whichever way it stops, what is said next plays: a pause that outlived
    /// what it paused would keep it silent, with no line to say why.
    #[test]
    fn a_pause_ends_with_what_it_paused() {
        let ended: [fn(&mut Aloud); 3] = [
            |aloud| aloud.press(Button::Stop),
            |aloud| drop(aloud.ended(1, Ok(()))),
            |aloud| {
                aloud.hear(here(Vec::new()), true);
                aloud.hear(here(vec![saying(1, Does::Stop(Stop {}))]), true);
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

    /// A button drawn just before what plays ended does not read what is
    /// selected, nor pause what comes next.
    #[test]
    fn a_button_with_nothing_left_to_do_does_nothing() {
        let mut aloud = Aloud::new(std::sync::mpsc::channel().0);
        for button in [Button::Pause, Button::GoOn] {
            aloud.press(button);
            assert!(!aloud.queue.paused);
            assert!(!aloud.queue.busy());
        }
    }

    /// One drawn before the pause changed does not do the opposite of what it
    /// shows.
    #[test]
    fn a_button_drawn_before_a_change_does_what_it_shows_or_nothing() {
        let (mut aloud, quiet) = aloud_reading();
        aloud.press(Button::GoOn);
        assert!(!quiet.held.load(Ordering::Relaxed), "what plays plays on");
        aloud.press(Button::Pause);
        aloud.press(Button::Pause);
        assert!(quiet.held.load(Ordering::Relaxed), "what is paused stays paused");
    }

    /// What `--stop` said before more was said ends the pause in the order said:
    /// what came after it plays.
    #[test]
    fn what_is_said_after_a_stop_is_not_paused() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.pause(at(1));
        let said = vec![saying(1, Does::Stop(Stop {})), in_turn(2, "after the stop", None)];
        queue.hear(here(said), true, at(2));
        assert!(!queue.paused);
        assert_eq!(spoken(&mut queue), ["after the stop"]);
    }

    /// `--now` takes the place of what is paused, and waits with it: an agent
    /// does not talk over the pause you asked for.
    #[test]
    fn what_goes_first_does_not_end_a_pause() {
        let mut queue = hearing();
        let quiet = reading(&mut queue, 1);
        queue.pause(at(1));
        queue.hear(here(vec![saying(1, Does::Now(utterance("first", None)))]), true, at(2));
        assert!(quiet.cut.load(Ordering::Relaxed));
        assert!(queue.paused);
        assert_eq!(spoken(&mut queue), ["first"]);
    }

    /// All that waited through a pause is read, each thing taking longer than
    /// `STALE` to read: one deadline for all of it would lose the last of it,
    /// the handoff an agent ended on.
    #[test]
    fn what_waited_through_a_pause_is_all_read_however_long_it_takes() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.pause(at(1));
        queue.hear(here(vec![saying(1, Does::Now(utterance("first", None)))]), true, at(1));
        let last = WAITING as u64 + 1;
        let rest = (2..=last).map(|n| in_turn(n, &n.to_string(), None)).collect();
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

    /// What is said behind what waited through a pause would have been read by
    /// now but for the pause, and is read too; and once that is all read, what
    /// is said next has an age of its own again.
    #[test]
    fn what_comes_behind_what_waited_through_a_pause_is_read_too() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "waited", None)]), true, at(1));
        queue.pause(at(1));
        queue.unpause();
        queue.hear(here(vec![in_turn(2, "behind", None)]), true, at(2));
        let late = at(2) + STALE * 5;
        assert_eq!(queue.next(late).map(|next| next.spoken).as_deref(), Some("waited"));
        assert_eq!(queue.next(late).map(|next| next.spoken).as_deref(), Some("behind"));
        queue.hear(here(vec![in_turn(3, "after", None)]), true, at(3));
        assert_eq!(queue.next(late), None, "it waited on nothing of yours");
    }

    /// What had waited too long as you paused goes, as it would have with no
    /// pause: a Mac that woke does not read out an hour of updates for two taps
    /// of a key. What had not stays.
    #[test]
    fn a_pause_does_not_bring_back_what_had_waited_too_long() {
        let mut queue = hearing();
        let early = vec![saying(1, Does::Now(utterance("first", None))), in_turn(2, "early", None)];
        queue.hear(here(early), true, at(1));
        queue.hear(here(vec![in_turn(3, "late", None)]), true, at(1) + STALE);
        drop(reading(&mut queue, 1));
        queue.pause(at(2) + STALE);
        queue.unpause();
        let later = at(3) + STALE * 5;
        assert_eq!(queue.next(later).map(|next| next.spoken).as_deref(), Some("late"));
        assert_eq!(queue.next(later), None);
    }

    /// With only what had waited too long, there is nothing to pause, and a
    /// pause of it would keep what is said next silent.
    #[test]
    fn a_pause_of_only_what_had_waited_too_long_is_none() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early", None)]), true, at(1));
        queue.pause(at(2) + STALE);
        assert!(!queue.paused);
        assert!(!queue.busy());
    }

    /// A button drawn before the pause, clicked long into it, lets go of nothing
    /// that waited through it.
    #[test]
    fn pausing_what_is_paused_lets_go_of_nothing() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "rest", None)]), true, at(1));
        queue.pause(at(1));
        queue.pause(at(2) + STALE * 5);
        assert!(queue.paused);
        assert_eq!(queue.in_turn.len(), 1);
    }

    /// What you paused is still there to go on, where what the pause held ends
    /// past `STALE`: as speakers that went, a `--now` with no words to say, or a
    /// model that came late, and the pause goes on with it.
    #[test]
    fn nothing_goes_stale_during_a_pause() {
        let mut queue = hearing();
        drop(reading(&mut queue, 1));
        queue.hear(here(vec![in_turn(1, "rest", None)]), true, at(1));
        queue.pause(at(1));
        assert!(queue.ended(1));
        assert!(queue.paused, "what waits is what you paused too");
        assert_eq!(queue.next(at(2) + STALE * 5).map(|next| next.spoken).as_deref(), Some("rest"));
    }

    /// What you did not pause did not wait on you: a button drawn before its
    /// pause ended gives it no more time.
    #[test]
    fn going_on_with_nothing_paused_gives_what_waits_no_more_time() {
        let mut queue = hearing();
        queue.hear(here(vec![in_turn(1, "early", None)]), true, at(1));
        queue.unpause();
        assert_eq!(queue.next(at(2) + STALE), None);
    }

    /// A click hits the button whose icon it is on, or nothing, at any width,
    /// down to where one icon fits or none does, and whatever an agent's words
    /// hold: an icon never shows on the other's spot, nor a spot with no icon.
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

    /// Control characters are not drawn, and leave no gap after the buttons.
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

    /// The sidebar as it starts, of its width less its edge, has room for how
    /// far the fetch of the model got, its buttons and all.
    #[test]
    fn the_fetch_says_how_far_it_got_at_the_width_the_sidebar_starts_at() {
        let shown = Shown::Fetching { got: 12_000_000, paused: false };
        let (line, _) = shown.line(usize::from(COLUMNS) - 1);
        let said: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        let far = format!("12 of {} MB", voice::BYTES / 1_000_000);
        assert!(said.contains(&far), "{said}");
    }

    /// A terminal that speaks kitty's keys, and one that types them.
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
    fn a_pane_s_frames_and_marks_are_not_read() {
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
        let from = |speaker| Whose::Said { on: far(), speaker };
        assert_eq!(line(Whose::Selection), format!("{MARK} selection · the tests pass"));
        let said = from(Some(pane("old", Some("luneta"))));
        assert_eq!(line(said), format!("{MARK} luneta · the tests pass"));
        assert_eq!(line(from(None)), format!("{MARK} far · the tests pass"));
        let untold = from(Some(pane("old", None)));
        assert_eq!(line(untold), format!("{MARK} old · the tests pass"));
    }

    /// A row is marked only where the daemon said what session its pane is in:
    /// the name a pane knows its session by may be another's now.
    #[test]
    fn what_comes_from_a_session_not_told_marks_no_row() {
        let shown = |speaker| {
            let whose = Whose::Said { on: far(), speaker: Some(speaker) };
            Shown::Reading { whose, text: String::new(), paused: false }
        };
        let told = shown(pane("old", Some("new")));
        assert_eq!(told.from(), Some((&far(), "new", 3)));
        assert_eq!(shown(pane("old", None)).from(), None);
    }

    /// A pane's session is named by what its daemon says it is now, and is the
    /// same speaker whichever name that goes by.
    #[test]
    fn a_session_not_told_goes_by_the_name_the_pane_knows_it_by() {
        let mut queue = hearing();
        let from = |text: &str, speaker| {
            Utterance { text: text.to_owned(), speaker: Some(speaker) }
        };
        let said = vec![
            saying(1, Does::InTurn(from("told", pane("old", Some("new"))))),
            saying(2, Does::InTurn(from("same", pane("other", Some("new"))))),
            saying(3, Does::InTurn(from("untold", pane("old", None)))),
        ];
        queue.hear(here(said), true, at(1));
        assert_eq!(spoken(&mut queue), ["new. told", "same", "old. untold"]);
    }
}
