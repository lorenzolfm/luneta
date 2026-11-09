//! Dictation, by `Alt r`: what you say, written where you would type it. Hold
//! the key and talk, and let it go: what you said is written in the pane
//! zellij is on, as a paste is, or on the ask box's line, where it has the
//! keys. A tap of the key keeps the microphone on until the next, to talk
//! hands free. `Enter` ends it too, and sends what was written, as `Enter`
//! would; `Esc` gives it up. Short of `Enter`, nothing is sent: what is
//! written waits for you, as what you type does.
//!
//! What you said goes in the pane zellij is on as it is written, as what
//! you type would: where something took zellij elsewhere meanwhile, as
//! another agent's `luneta go` does, or the pane ended, it goes there. zellij
//! tells luneta of no such move, and asking it each time would hold every
//! dictation up for a move few make.
//!
//! A box along the bottom of zellij shows what the model makes of what you
//! say as you say it, and how loud the microphone hears you. It has the keys
//! and the mouse until it closes, as every box over zellij does, so that
//! nothing moves what it writes into from under it. Of the keys, it takes
//! its own, and does nothing with the rest. luneta tells a hold from a tap by
//! when the key is let go, which only a terminal that speaks the kitty
//! keyboard protocol tells it: in one that does not, the key taps, and what
//! it presses again as the key is held is the key repeating, which does
//! nothing.
//!
//! Into a pane with an agent in it, a skill you name is written as its slash
//! command, as `named` has it, and the box shows it so as you say it: in the
//! harness's spelling once written, `/skill:<name>` for pi's. The finder says
//! which skills there are while you talk: what you said waits on it only if it
//! has a mark in it, a word that can name a skill.
//! `Enter` sends what the box showed as it went down: a command it did not show
//! then is written, and waits for you. Unlike words, a command is not written
//! where that agent has since gone from the pane: what you said is, as you said
//! it.
//!
//! The first time, the model is fetched, which the box says as it goes. The
//! fetch goes on with the box closed, and dictation waits for it.

use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::brief;
use crate::menu::{self, Hint};
use crate::named::{self, Written};
use crate::sidebar::{self, ACCENT};
use crate::models::Fetched;
use crate::skills::{Name, Skill, Skills};
use crate::speech::{self, Listening, Spoken};

/// The key that dictates, held or tapped.
pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('r'), KeyModifiers::ALT);

/// The longest the key is held for a tap: one let go after this long was
/// held to talk. As hex has it.
const TAP: Duration = Duration::from_millis(300);

/// How soon after it last went down, or repeated, the key that dictates
/// pressed again is the key held down, repeating, where the terminal does not
/// say so: a keyboard repeats a key held every 30 to 90 ms, and no one taps
/// to talk and taps again to be done as soon.
const REPEATS: Duration = Duration::from_millis(150);

/// What the rule along the box's top names it.
const TITLE: &str = "Dictate";

/// What the box says from when you are done until what you said is written.
const WRITING: &str = "Writing what you said…";

/// What the box says before the skills named after the first that only you can
/// run: an agent cannot.
const ONLY_FIRST: &str = "Runs only if named first:";

/// How many of the last levels of the microphone the box shows.
const LEVELS: usize = 8;

/// The glyphs of a level, from silence to the loudest.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// What you said, for luneta to write where the box was opened over.
#[derive(Debug, PartialEq, Eq)]
pub struct Dictated {
    /// What you said, as you said it.
    pub said: String,
    pub written: Written,
    /// Whether it is sent once written, as `Enter` ended the dictation.
    pub sends: bool,
}

impl Dictated {
    /// What was `said`, as it is written, what it names of `skills` too, for a
    /// dictation that ended as `ending` says.
    fn of(said: String, skills: &Skills, ending: &Ending) -> Self {
        let written = written(&said, skills);
        let sends = ending.sends(&written);
        Dictated { said, written, sends }
    }

    /// As it is written where the agent it was dictated to has gone from the
    /// pane: a command goes back to what you said, which waits for you, as
    /// the box showed you no such thing. Words go there as they are.
    pub fn moved(self) -> Self {
        if self.written.command.is_none() {
            return self;
        }
        let written = Written::said(&self.said);
        Dictated { said: self.said, written, sends: false }
    }
}

/// When a key that does something held down last went down or repeated, by
/// which luneta tells it held down from pressed again, where the terminal does
/// not say:
/// one that does not speak the kitty keyboard protocol presses the key held
/// again and again, as it would type it, and says no key let go.
#[derive(Debug)]
pub struct Repeats {
    /// The key it tells of.
    key: (KeyCode, KeyModifiers),
    last: Option<Instant>,
}

impl Repeats {
    pub fn of(key: (KeyCode, KeyModifiers)) -> Repeats {
        Repeats { key, last: None }
    }

    /// `key` at `now`, as it is, or, where it is the key this tells of
    /// pressed as soon after it last went down as a key held repeats, as the
    /// repeat it is, as kitty's keys say it.
    pub fn read(&mut self, key: KeyEvent, now: Instant) -> KeyEvent {
        if (key.code, key.modifiers) != self.key || key.kind == KeyEventKind::Release {
            return key;
        }
        let soon = self.last.is_some_and(|last| now.saturating_duration_since(last) < REPEATS);
        self.last = Some(now);
        match key.kind {
            KeyEventKind::Press if soon => KeyEvent { kind: KeyEventKind::Repeat, ..key },
            _ => key,
        }
    }
}

/// What a key does to the box, for luneta to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    /// Close the box, which gives up the dictation, if it is at one.
    Close,
    /// Close the box, which says something, and dictate.
    Dictate,
}

/// The box, from when `KEY` opens it until it closes.
#[derive(Debug)]
pub struct Dictation {
    /// Its number among the dictations luneta has opened, which what the
    /// model says of it carries.
    number: u64,
    stage: Stage,
}

/// Where the box is at.
#[derive(Debug)]
enum Stage {
    /// The model is on its way, as the box says: this many bytes of it are in.
    Fetching(u64),
    /// The model came.
    Fetched,
    Hearing(Hearing),
    /// The model wrote what you said, which has a mark in it, and the finder
    /// has yet to say what it can name. It is written then, as `ending` says.
    Naming { said: String, ending: Ending },
    /// What went wrong: why the dictation wrote nothing, or why the model
    /// did not come.
    Wrong(String),
}

/// A dictation, from when the key went down until what you said is written.
#[derive(Debug)]
struct Hearing {
    key: Key,
    /// What the model says of itself.
    model: Model,
    /// The last few levels of the microphone, the last last.
    levels: Vec<f32>,
    /// What the model makes of what you said so far.
    words: String,
    /// The skills what you say can name: none, where it is written into what
    /// runs none.
    skills: Skills,
    /// The dictation the model is at: dropped, it is given up.
    listening: Listening,
}

/// Where the key that dictates is at.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    /// Down since then, held as you talk, until it is let go.
    Down(Instant),
    /// Tapped: the microphone stays on until the key goes down again.
    Tapped,
    /// You are done: the model writes what you said, as this says.
    Done(Ending),
}

/// How you ended a dictation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ending {
    /// What you said is written, and waits for you.
    Writes,
    /// `Enter` ended it, with the box showing this command, or none: what you
    /// said is written and sent, but for another command than that, as the
    /// words the model writes last can make it.
    Sends { showed: Option<Name> },
}

impl Ending {
    /// Whether `written` is sent, once it is written.
    fn sends(&self, written: &Written) -> bool {
        matches!(self, Ending::Sends { showed } if *showed == written.command)
    }
}

/// What the model says of itself, as the dictation goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    /// The microphone is opening.
    Opening,
    /// The model is being read in, and hears what you say once it is.
    Loading,
    /// The model hears you, since then.
    Listening(Instant),
}

impl Dictation {
    /// The box numbered `number`, for the dictation `listening`, from
    /// `now`, as the key went down, of what can name `skills`. Where luneta's
    /// terminal tells it when a key is let go, as `releases` says, the key is
    /// held until then; else it was tapped.
    pub fn hearing(
        number: u64,
        listening: Listening,
        skills: Skills,
        releases: bool,
        now: Instant,
    ) -> Self {
        let key = if releases { Key::Down(now) } else { Key::Tapped };
        let hearing = Hearing {
            key,
            model: Model::Opening,
            levels: Vec::new(),
            words: String::new(),
            skills,
            listening,
        };
        Dictation { number, stage: Stage::Hearing(hearing) }
    }

    /// The box numbered `number`, while the model is on its way: `got` bytes
    /// of it are in.
    pub fn fetching(number: u64, got: u64) -> Self {
        Dictation { number, stage: Stage::Fetching(got) }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    /// What `key` does, at `now`. While the box hears you, `KEY` let go
    /// after a hold, or down again after a tap, has the model write what you
    /// said, and so does `Enter`, which then sends it. `Esc` and `Ctrl c`
    /// close the box, and give the dictation up. Any key that goes down
    /// closes a box that says something, as of the model's fetch, and `KEY`
    /// then dictates. A key held down that repeats does nothing.
    pub fn key(&mut self, key: &KeyEvent, now: Instant) -> Keyed {
        let pressed = key.kind == KeyEventKind::Press;
        let chord = (key.code, key.modifiers);
        let closes = chord == (KeyCode::Esc, KeyModifiers::NONE)
            || chord == (KeyCode::Char('c'), KeyModifiers::CONTROL);
        let hearing = match &mut self.stage {
            Stage::Hearing(hearing) => hearing,
            Stage::Naming { .. } => {
                return if pressed && closes { Keyed::Close } else { Keyed::Nothing };
            },
            Stage::Fetching(_) | Stage::Fetched | Stage::Wrong(_) => {
                return match (pressed, chord == KEY) {
                    (true, true) => Keyed::Dictate,
                    (true, false) => Keyed::Close,
                    (false, _) => Keyed::Nothing,
                };
            },
        };
        if pressed && closes {
            return Keyed::Close;
        }
        match (&hearing.key, key.kind) {
            // Let go, with or without `Alt`, which may go first.
            (Key::Down(down), KeyEventKind::Release) if key.code == KEY.0 => {
                if now.saturating_duration_since(*down) < TAP {
                    hearing.key = Key::Tapped;
                } else {
                    hearing.finish(Ending::Writes);
                }
            },
            // A terminal that tells no key let go repeats a key held as it
            // goes down again: so a hold there ends as the key repeats.
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press) if chord == KEY => {
                hearing.finish(Ending::Writes);
            },
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press)
                if chord == (KeyCode::Enter, KeyModifiers::NONE) =>
            {
                let showed = written(hearing.words.trim(), &hearing.skills).command;
                hearing.finish(Ending::Sends { showed });
            },
            _ => {},
        }
        Keyed::Nothing
    }

    /// luneta's window lost the focus: a key held there is let go elsewhere,
    /// which luneta never hears, so the model writes what you said.
    pub fn unfocused(&mut self) {
        if let Stage::Hearing(hearing) = &mut self.stage
            && matches!(hearing.key, Key::Down(_))
        {
            hearing.finish(Ending::Writes);
        }
    }

    /// Takes in what the model said of the dictation, and gives what you
    /// said to be written, once it is over. A dictation given up says
    /// nothing more.
    pub fn spoken(&mut self, spoken: Spoken, now: Instant) -> Option<Dictated> {
        let Stage::Hearing(hearing) = &mut self.stage else {
            return None;
        };
        match spoken {
            Spoken::Loading => hearing.model = Model::Loading,
            Spoken::Listening => hearing.model = Model::Listening(now),
            Spoken::Level(level) => {
                if hearing.levels.len() == LEVELS {
                    hearing.levels.remove(0);
                }
                hearing.levels.push(level);
            },
            Spoken::Words(words) => hearing.words = words,
            // A dictation that went on as long as one goes ends with the key
            // still down, or tapped, and is not sent.
            Spoken::Ended(Ok(text)) => {
                let ending = match &hearing.key {
                    Key::Done(ending) => ending.clone(),
                    Key::Down(_) | Key::Tapped => Ending::Writes,
                };
                if hearing.skills == Skills::Finding && named::marked(&text) {
                    self.stage = Stage::Naming { said: text, ending };
                    return None;
                }
                return Some(Dictated::of(text, &hearing.skills, &ending));
            },
            Spoken::Ended(Err(why)) => self.stage = Stage::Wrong(why),
        }
        None
    }

    /// Takes in the skills the finder found, or why it found none, and gives
    /// what you said to be written, if it waited on them.
    pub fn found(&mut self, skills: Result<Vec<Skill>, String>) -> Option<Dictated> {
        let skills = match skills {
            Ok(skills) => Skills::Found(skills),
            Err(why) => Skills::Failed(why),
        };
        match &mut self.stage {
            Stage::Hearing(hearing) => {
                hearing.skills = skills;
                None
            },
            Stage::Naming { said, ending } => Some(Dictated::of(said.clone(), &skills, ending)),
            Stage::Fetching(_) | Stage::Fetched | Stage::Wrong(_) => None,
        }
    }

    /// Takes in how the model's fetch goes.
    pub fn fetched(&mut self, fetched: &Fetched) {
        match (&mut self.stage, fetched) {
            (Stage::Fetching(got), Fetched::Got(now)) => *got = *now,
            (Stage::Fetching(_), Fetched::Ended(Ok(()))) => self.stage = Stage::Fetched,
            (Stage::Fetching(_), Fetched::Ended(Err(why))) => {
                self.stage = Stage::Wrong(format!("The speech model did not come: {why}"));
            },
            _ => {},
        }
    }

    /// Whether its spinner turns, and its clock: while it hears you, or
    /// writes what you said.
    pub fn turning(&self) -> bool {
        matches!(self.stage, Stage::Hearing(_) | Stage::Naming { .. })
    }

    /// Draws it in a box along the bottom of `over`, zellij's columns, at
    /// `now`, as wide as `brief::room` says, as tall as its lines. The rule
    /// along its top says how long it has heard you, and how loud.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: (SystemTime, Instant)) {
        let Some((inner, room)) = brief::room(over) else {
            return;
        };
        let (lines, keys, right) = match &self.stage {
            Stage::Fetching(got) => fetching(*got, inner),
            Stage::Fetched => {
                let said = "The speech model is in. Hold Alt r and talk, or tap it to talk \
                    with your hands free.";
                let lines = menu::wrap(said, inner, Style::new());
                let keys = vec![(Hint::Alt('r'), "Dictate"), (Hint::AnyOtherKey, "Close")];
                (lines, keys, String::new())
            },
            Stage::Wrong(why) => wrong(why, inner),
            Stage::Hearing(hearing) => hearing.lines(inner, now),
            Stage::Naming { said, .. } => {
                let mut lines = menu::wrap(said.trim(), inner, Style::new());
                lines.push(Line::default());
                let dim = Style::new().add_modifier(Modifier::DIM);
                lines.extend(menu::waiting(WRITING, inner, dim, sidebar::frame(now.0)));
                (lines, vec![(Hint::Esc, "Cancel")], String::new())
            },
        };
        let mut lines = lines;
        let footer = [Line::default(), menu::keys_line(inner, &keys)];
        let room = room.saturating_sub(footer.len());
        // What the model made of the first of what you said goes, where the
        // box has no room for it all.
        if lines.len() > room {
            lines.drain(..lines.len() - room);
        }
        lines.extend(footer);
        let width = u16::try_from(inner + 2).unwrap_or(over.width).min(over.width);
        let height = u16::try_from(lines.len() + 2).unwrap_or(over.height).min(over.height);
        let x = over.x + (over.width - width) / 2;
        // A line clear of zellij's last, where its bar is.
        let y = (over.bottom().saturating_sub(height + 1)).max(over.y);
        let area = Rect::new(x, y, width, height);
        menu::boxed(buffer, area, &menu::rule(TITLE, &right, inner), &lines);
    }
}

impl Hearing {
    /// Has the model write what you said, as `ending` says.
    fn finish(&mut self, ending: Ending) {
        self.key = Key::Done(ending);
        self.listening.finish();
    }

    /// Its lines, `inner` wide, at `now`, the keys under them, and what its
    /// rule says on the right: how long the model has heard you, and how
    /// loud the microphone heard you last. What you said so far shows as it
    /// would be written, what it names of its skills too, and any it names
    /// that would not run.
    fn lines(&self, inner: usize, now: (SystemTime, Instant)) -> Shown {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let frame = sidebar::frame(now.0);
        let written = written(self.words.trim(), &self.skills);
        let words = written.to_string();
        let mut lines = menu::wrap(&words, inner, Style::new());
        if words.is_empty() {
            lines.clear();
        }
        if !written.only_first.is_empty() {
            let only = written.only_first.iter().map(|name| format!("/{name}"));
            let said = format!("{ONLY_FIRST} {}", only.collect::<Vec<_>>().join(", "));
            lines.push(Line::default());
            lines.extend(menu::wrap(&said, inner, Style::new().fg(sidebar::ERROR)));
        }
        let doing = match self.model {
            _ if matches!(self.key, Key::Done(_)) => Some(WRITING),
            Model::Opening => Some("Opening the microphone…"),
            Model::Loading => Some("Reading the speech model in: go on talking…"),
            Model::Listening(_) if lines.is_empty() => {
                lines.push(Line::styled(" Listening…", dim));
                None
            },
            Model::Listening(_) => None,
        };
        if let Some(doing) = doing {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(menu::waiting(doing, inner, dim, frame));
        }
        let keys = match self.key {
            Key::Down(_) => vec![(Hint::Alt('r'), "Let go to write"), (Hint::Esc, "Cancel")],
            Key::Tapped => vec![
                (Hint::Alt('r'), "Write"),
                (Hint::Enter, "Write and send"),
                (Hint::Esc, "Cancel"),
            ],
            Key::Done(_) => vec![(Hint::Esc, "Cancel")],
        };
        let heard = match self.model {
            Model::Listening(since) => now.1.saturating_duration_since(since),
            Model::Opening | Model::Loading => Duration::ZERO,
        };
        let seconds = heard.as_secs();
        let bars: String = self.levels.iter().map(|level| bar(*level)).collect();
        (lines, keys, format!("● {}:{:02} {bars}", seconds / 60, seconds % 60))
    }
}

/// What was `said`, as it is written, what it names of `skills`, once the finder
/// has said them, as what runs it.
fn written(said: &str, skills: &Skills) -> Written {
    match skills {
        Skills::Found(skills) => named::written(said, skills),
        Skills::Finding | Skills::Failed(_) => Written::said(said),
    }
}

/// A box's lines, the keys under them, and what its rule says on the right.
type Shown = (Vec<Line<'static>>, Vec<(Hint, &'static str)>, String);

/// The lines of the box while the model is on its way, `inner` wide: `got`
/// bytes of it are in.
fn fetching(got: u64, inner: usize) -> Shown {
    let said = format!(
        "luneta fetches its speech model, once: {} of {} MB, from Hugging Face. It goes on \
         with this box closed. Dictate once it is in.",
        got / 1_000_000,
        speech::BYTES / 1_000_000
    );
    let mut lines = menu::wrap(&said, inner, Style::new());
    lines.push(Line::default());
    lines.push(progress(got, speech::BYTES, inner));
    let percent = got.saturating_mul(100) / speech::BYTES.max(1);
    (lines, vec![(Hint::AnyKey, "Close")], format!("{percent}%"))
}

/// What went wrong, `inner` wide, in red, which any key closes, and `KEY`
/// tries again.
fn wrong(why: &str, inner: usize) -> Shown {
    let lines = brief::paragraphs(why, inner, Style::new().fg(sidebar::ERROR));
    (lines, vec![(Hint::Alt('r'), "Try again"), (Hint::AnyOtherKey, "Close")], String::new())
}

/// A bar `inner` wide, as full as `got` is of `of`.
fn progress(got: u64, of: u64, inner: usize) -> Line<'static> {
    let room = inner.saturating_sub(2);
    let full = usize::try_from(got.min(of) * room as u64 / of.max(1)).unwrap_or(room);
    Line::from(vec![
        Span::raw(" "),
        Span::styled("━".repeat(full), Style::new().fg(ACCENT)),
        Span::styled("━".repeat(room - full), Style::new().add_modifier(Modifier::DIM)),
    ])
}

/// The glyph of a level of the microphone, from 0 to 1.
fn bar(level: f32) -> char {
    let at = (level.clamp(0.0, 1.0) * (BARS.len() - 1) as f32).round() as usize;
    BARS[at.min(BARS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, TryRecvError};

    use super::*;
    use crate::gruvbox;
    use crate::skills::Runs;

    fn alt_r(kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(KeyCode::Char('r'), KeyModifiers::ALT, kind)
    }

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// A box that hears you from `at`, in a terminal that tells when a key
    /// is let go, as `releases` says, and what the thread that hears you is
    /// told: to finish, or, as it drops, to give up.
    fn hearing(releases: bool, at: Instant) -> (Dictation, Receiver<()>) {
        hearing_of(Skills::Found(Vec::new()), releases, at)
    }

    /// As `hearing`, for what can name `skills`.
    fn hearing_of(skills: Skills, releases: bool, at: Instant) -> (Dictation, Receiver<()>) {
        let (listening, stop) = Listening::new();
        (Dictation::hearing(1, listening, skills, releases, at), stop)
    }

    fn skills(names: &[&str]) -> Vec<Skill> {
        let skill = |name: &&str| Skill {
            name: Name::new((*name).to_owned()).expect("a name"),
            description: String::new(),
            runs: Runs::ForAnyone,
        };
        names.iter().map(skill).collect()
    }

    fn ended(dictation: &mut Dictation, text: &str) -> Option<Dictated> {
        dictation.spoken(Spoken::Ended(Ok(text.to_owned())), Instant::now())
    }

    fn written(text: &str, sends: bool) -> Dictated {
        Dictated { said: text.to_owned(), written: Written::said(text), sends }
    }

    /// As `written`, for what was `said`, but for the slash command of the
    /// skill `name` first.
    fn runs(said: &str, name: &str, text: &str, sends: bool) -> Dictated {
        let command = Name::new(name.to_owned());
        let written = Written { command, text: text.to_owned(), only_first: Vec::new() };
        Dictated { said: said.to_owned(), written, sends }
    }

    /// The key held as you talk repeats, and does nothing, until it is let
    /// go, `Alt` first or not: then the model writes what you said.
    #[test]
    fn a_hold_writes_what_you_said_as_the_key_is_let_go() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        let held = at + Duration::from_millis(100);
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Repeat), held), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        let let_go =
            KeyEvent::new_with_kind(KeyCode::Char('r'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(dictation.key(&let_go, at + Duration::from_secs(2)), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Ok(()));
        assert_eq!(ended(&mut dictation, "Open a tab."), Some(written("Open a tab.", false)));
    }

    /// A key let go as fast as a tap leaves the microphone on, whatever you
    /// type, until the key goes down again.
    #[test]
    fn a_tap_listens_until_the_key_goes_down_again() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        let tapped = at + Duration::from_millis(150);
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Release), tapped), Keyed::Nothing);
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(dictation.key(&typed, at + Duration::from_secs(1)), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        let again = at + Duration::from_secs(9);
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Press), again), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Ok(()));
    }

    /// In a terminal that tells no key let go, the key taps.
    #[test]
    fn where_no_key_is_told_let_go_the_key_taps() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(false, at);
        dictation.unfocused();
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Press), at), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Ok(()));
    }

    /// A terminal that says no key repeats presses the key held again and
    /// again: the first press past the keyboard's delay is a press, which is
    /// done with a tap, and the presses as soon after as a key repeats are
    /// its repeats, until a press after a pause. Other keys are as they are.
    #[test]
    fn the_key_pressed_as_soon_after_as_a_key_repeats_is_held() {
        let at = Instant::now();
        let mut repeats = Repeats::of(KEY);
        let mut read = |key: KeyEvent, after: u64| {
            repeats.read(key, at + Duration::from_millis(after)).kind
        };
        let press = alt_r(KeyEventKind::Press);
        assert_eq!(read(press, 0), KeyEventKind::Press);
        assert_eq!(read(press, 500), KeyEventKind::Press);
        assert_eq!(read(press, 530), KeyEventKind::Repeat);
        assert_eq!(read(alt_r(KeyEventKind::Repeat), 560), KeyEventKind::Repeat);
        assert_eq!(read(press, 590), KeyEventKind::Repeat);
        assert_eq!(read(alt_r(KeyEventKind::Release), 600), KeyEventKind::Release);
        assert_eq!(read(press, 2_000), KeyEventKind::Press);
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(read(typed, 2_010), KeyEventKind::Press);
    }

    #[test]
    fn enter_writes_what_you_said_and_sends_it() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(false, at);
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(dictation.key(&enter, at), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Ok(()));
        assert_eq!(ended(&mut dictation, "Run the tests."), Some(written("Run the tests.", true)));
    }

    #[test]
    fn esc_and_ctrl_c_give_the_dictation_up() {
        let at = Instant::now();
        for key in [KeyCode::Esc, KeyCode::Char('c')] {
            let (mut dictation, stop) = hearing(true, at);
            let modifiers =
                if key == KeyCode::Esc { KeyModifiers::NONE } else { KeyModifiers::CONTROL };
            assert_eq!(dictation.key(&pressed(key, modifiers), at), Keyed::Close);
            drop(dictation);
            assert_eq!(stop.try_recv(), Err(TryRecvError::Disconnected));
        }
    }

    /// The key held as luneta's window loses the focus is let go where
    /// luneta never hears it.
    #[test]
    fn a_key_held_as_the_window_loses_the_focus_writes_what_you_said() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        dictation.unfocused();
        assert_eq!(stop.try_recv(), Ok(()));
    }

    /// What went wrong shows until a key goes down: not the one that goes up
    /// as the dictation ends.
    #[test]
    fn what_went_wrong_shows_until_a_key_goes_down() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing(true, at);
        assert_eq!(dictation.spoken(Spoken::Ended(Err(speech::UNHEARD.to_owned())), at), None);
        assert!(!dictation.turning());
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Release), at), Keyed::Nothing);
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(dictation.key(&typed, at), Keyed::Close);
    }

    #[test]
    fn the_key_on_a_box_that_says_something_dictates() {
        let at = Instant::now();
        let mut dictation = Dictation::fetching(1, 0);
        dictation.fetched(&Fetched::Ended(Ok(())));
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Release), at), Keyed::Nothing);
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Press), at), Keyed::Dictate);
    }

    /// A row of zellij's, 60 wide, with no box over it.
    const Z: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

    /// The box over `width` by `height` columns of zellij, drawn at `now`, a
    /// line for each row, `z` where zellij shows through, in luneta's colours.
    fn shown(dictation: &Dictation, width: u16, height: u16, now: Instant) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        dictation.draw(&mut buffer, area, (SystemTime::UNIX_EPOCH, now));
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    /// Along zellij's bottom, above its last line: what the model made of
    /// what you said so far, under a rule that says how long it heard you,
    /// and how loud, and the keys.
    #[test]
    fn the_box_shows_what_you_said_as_you_say_it() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing(true, at);
        dictation.spoken(Spoken::Listening, at);
        for level in [0.0, 0.5, 1.0] {
            dictation.spoken(Spoken::Level(level), at);
        }
        let words = "Open a new tab in the luneta session and run the tests";
        dictation.spoken(Spoken::Words(words.to_owned()), at);
        assert_eq!(
            shown(&dictation, 60, 9, at + Duration::from_secs(64)),
            [
                Z,
                Z,
                Z,
                "┌─ Dictate ─────────────────────────────────── ● 1:04 ▁▅█ ─┐",
                "│ Open a new tab in the luneta session and run the tests   │",
                "│                                                          │",
                "│ <Alt r> - Let go to write, <ESC> - Cancel                │",
                "└──────────────────────────────────────────────────────────┘",
                Z,
            ]
        );
    }

    /// You see the command as it will be written, before `Enter` sends it,
    /// and what `Enter` sends is that.
    #[test]
    fn the_box_shows_a_skill_named_as_its_command() {
        let at = Instant::now();
        let named = Skills::Found(skills(&["thermo-nuclear-review"]));
        let (mut dictation, stop) = hearing_of(named, false, at);
        dictation.spoken(Spoken::Listening, at);
        let said = "review this with the thermonuclear review skill";
        dictation.spoken(Spoken::Words(said.to_owned()), at);
        let lines = shown(&dictation, 60, 7, at);
        assert_eq!(lines[2], "│ /thermo-nuclear-review review this                       │");
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(dictation.key(&enter, at), Keyed::Nothing);
        assert_eq!(stop.try_recv(), Ok(()));
        let said = "Review this with the thermonuclear review skill.";
        assert_eq!(
            ended(&mut dictation, said),
            Some(runs(said, "thermo-nuclear-review", "Review this", true))
        );
    }

    /// `Enter` sends what the box showed as it went down, in words a little
    /// other than the box's, as the model writes them last. A command it did
    /// not show, or one it showed that is no more, is written, and not sent.
    #[test]
    fn enter_sends_only_the_command_the_box_showed() {
        let at = Instant::now();
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        for (shown, sent) in [
            ("Run the tests", written("Run all the tests.", true)),
            ("slash tiger style", runs("Slash tiger style, now.", "tigerstyle", "now.", true)),
            (
                "Review the tiger style",
                runs("Review the tiger style skill.", "tigerstyle", "Review", false),
            ),
            ("slash tiger style", written("Slash tie your style.", false)),
        ] {
            let named = Skills::Found(skills(&["tigerstyle"]));
            let (mut dictation, _stop) = hearing_of(named, false, at);
            dictation.spoken(Spoken::Words(shown.to_owned()), at);
            dictation.key(&enter, at);
            let said = sent.said.clone();
            assert_eq!(ended(&mut dictation, &said), Some(sent), "{shown:?}");
        }
    }

    /// Where the agent has gone from the pane, a command goes back to what you
    /// said, and is not sent, whatever the box showed. Words go as they are.
    #[test]
    fn a_command_goes_back_to_words_where_the_agent_has_gone() {
        let said = "Slash tiger style, now.";
        assert_eq!(runs(said, "tigerstyle", "now.", true).moved(), written(said, false));
        assert_eq!(written("Run the tests.", true).moved(), written("Run the tests.", true));
    }

    /// Named after the first, a skill only you can run would not run, which
    /// the box says.
    #[test]
    fn the_box_says_which_skills_run_only_if_named_first() {
        let at = Instant::now();
        let mut named = skills(&["handoff", "tigerstyle"]);
        named[0].runs = Runs::OnlyForYou;
        let (mut dictation, _stop) = hearing_of(Skills::Found(named), false, at);
        dictation.spoken(Spoken::Listening, at);
        let said = "slash tiger style and then the handoff skill";
        dictation.spoken(Spoken::Words(said.to_owned()), at);
        let lines = shown(&dictation, 60, 9, at);
        assert_eq!(lines[2], "│ /tigerstyle and then the handoff skill                   │");
        assert_eq!(lines[4], "│ Runs only if named first: /handoff                       │");
    }

    /// What you said, and it names a skill, waits for the finder, with the
    /// box open. It then writes what the finder's skills make of it, and does
    /// not send it, though `Enter` ended it, as the box showed it no command
    /// then. Only `Esc` and `Ctrl c` do anything to it meanwhile.
    #[test]
    fn what_may_name_a_skill_waits_for_the_finder() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing_of(Skills::Finding, false, at);
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        dictation.key(&enter, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        assert!(dictation.turning());
        let lines = shown(&dictation, 60, 9, at);
        assert_eq!(lines[2], "│ Slash hand off.                                          │");
        assert!(lines[4].contains("Writing what you said…"), "{lines:?}");
        assert_eq!(dictation.key(&enter, at), Keyed::Nothing);
        assert_eq!(dictation.key(&alt_r(KeyEventKind::Press), at), Keyed::Nothing);
        let found = dictation.found(Ok(skills(&["handoff"])));
        assert_eq!(found, Some(runs("Slash hand off.", "handoff", "", false)));

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        let esc = pressed(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(dictation.key(&esc, at), Keyed::Close);
    }

    /// A finder that fails leaves what you said as you said it, and nothing
    /// waits on one that is yet to say, where no word can name a skill. An
    /// answer with nothing waiting on it just fills the box.
    #[test]
    fn the_finder_holds_up_only_what_it_can_change() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        let failed = Err("skills: no answer".to_owned());
        assert_eq!(dictation.found(failed), Some(written("Slash hand off.", false)));

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(ended(&mut dictation, "Run the tests."), Some(written("Run the tests.", false)));

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(dictation.found(Ok(skills(&["handoff"]))), None);
        let said = "Slash hand off.";
        assert_eq!(ended(&mut dictation, said), Some(runs(said, "handoff", "", false)));
    }

    #[test]
    fn the_box_says_how_the_fetch_goes() {
        let mut dictation = Dictation::fetching(1, 0);
        dictation.fetched(&Fetched::Got(speech::BYTES / 4));
        assert_eq!(
            shown(&dictation, 60, 12, Instant::now()),
            [
                Z,
                Z,
                "┌─ Dictate ────────────────────────────────────────── 25% ─┐",
                "│ luneta fetches its speech model, once: 182 of 731 MB,    │",
                "│ from Hugging Face. It goes on with this box closed.      │",
                "│ Dictate once it is in.                                   │",
                "│                                                          │",
                "│ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ │",
                "│                                                          │",
                "│ <any key> - Close                                        │",
                "└──────────────────────────────────────────────────────────┘",
                Z,
            ]
        );
    }
}
