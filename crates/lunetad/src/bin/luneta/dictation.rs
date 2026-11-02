//! Dictation, by `Alt r`: what you say, written where you would type it. Hold
//! the key and talk, and let it go: what you said is written in the pane
//! zellij is on, as a paste is, or on the ask box's line, where it has the
//! keys. A tap of the key keeps the microphone on until the next, to talk
//! hands free. `Enter` ends it too, and sends what was written, as `Enter`
//! would; `Esc` gives it up. Short of `Enter`, nothing is sent: what is
//! written waits for you, as what you type does.
//!
//! A box along the bottom of zellij shows what the model makes of what you
//! say as you say it, and how loud the microphone hears you. It has the keys
//! until it closes, and takes no other. luneta tells a hold from a tap by
//! when the key is let go, which only a terminal that speaks the kitty
//! keyboard protocol tells it: in one that does not, the key taps.
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
use crate::sidebar::{self, ACCENT};
use crate::speech::{self, Fetched, Listening, Spoken};

/// The key that dictates, held or tapped.
pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('r'), KeyModifiers::ALT);

/// The longest the key is held for a tap: one let go after this long was
/// held to talk. As hex has it.
const TAP: Duration = Duration::from_millis(300);

/// What the rule along the box's top names it.
const TITLE: &str = "Dictate";

/// How many of the last levels of the microphone the box shows.
const LEVELS: usize = 8;

/// The glyphs of a level, from silence to the loudest.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Where what you said is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// In the pane zellij is on.
    Zellij,
    /// On the line of the ask box of this number.
    Ask(u64),
}

/// What you said, for luneta to write.
#[derive(Debug, PartialEq, Eq)]
pub struct Dictated {
    pub into: Where,
    pub text: String,
    /// Whether it is sent once written, as `Enter` ended the dictation.
    pub sends: bool,
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
pub struct Dictation {
    /// Its number among the dictations luneta has opened, which what the
    /// model says of it carries.
    number: u64,
    stage: Stage,
}

/// Where the box is at.
enum Stage {
    /// The model is on its way, as the box says: this many bytes of it are in.
    Fetching(u64),
    /// The model came, or why it did not.
    Fetched(Result<(), String>),
    Hearing(Hearing),
    /// Why the dictation wrote nothing.
    Wrong(String),
}

/// A dictation, from when the key went down until what you said is written.
struct Hearing {
    into: Where,
    key: Key,
    /// What the model says of itself.
    model: Model,
    /// The last few levels of the microphone, the last last.
    levels: Vec<f32>,
    /// What the model makes of what you said so far.
    words: String,
    /// Since when the model hears you.
    since: Option<Instant>,
    /// Whether what you said is sent once written.
    sends: bool,
    /// The dictation the model is at: dropped, it is given up.
    listening: Listening,
}

/// Where the key that dictates is at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// Down since then, held as you talk, until it is let go.
    Down(Instant),
    /// Tapped: the microphone stays on until the key goes down again.
    Tapped,
    /// You are done: the model writes what you said.
    Done,
}

/// What the model says of itself, as the dictation goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    /// The microphone is opening.
    Opening,
    /// The model is being read in, and hears what you say once it is.
    Loading,
    Listening,
}

impl Dictation {
    /// The box numbered `number`, for the dictation `listening`, into
    /// `into`, from `now`, as the key went down. Where luneta's terminal
    /// tells it when a key is let go, as `releases` says, the key is held
    /// until then; else it was tapped.
    pub fn hearing(
        number: u64,
        into: Where,
        listening: Listening,
        releases: bool,
        now: Instant,
    ) -> Self {
        let key = if releases { Key::Down(now) } else { Key::Tapped };
        let hearing = Hearing {
            into,
            key,
            model: Model::Opening,
            levels: Vec::new(),
            words: String::new(),
            since: None,
            sends: false,
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
        let Stage::Hearing(hearing) = &mut self.stage else {
            return match (pressed, chord == KEY) {
                (true, true) => Keyed::Dictate,
                (true, false) => Keyed::Close,
                (false, _) => Keyed::Nothing,
            };
        };
        let closes = chord == (KeyCode::Esc, KeyModifiers::NONE)
            || chord == (KeyCode::Char('c'), KeyModifiers::CONTROL);
        if pressed && closes {
            return Keyed::Close;
        }
        match (hearing.key, key.kind) {
            // Let go, with or without `Alt`, which may go first.
            (Key::Down(down), KeyEventKind::Release) if key.code == KEY.0 => {
                if now.saturating_duration_since(down) < TAP {
                    hearing.key = Key::Tapped;
                } else {
                    hearing.finish(false);
                }
            },
            // A terminal that tells no key let go repeats a key held as it
            // goes down again: so a hold there ends as the key repeats.
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press) if chord == KEY => {
                hearing.finish(false);
            },
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press)
                if chord == (KeyCode::Enter, KeyModifiers::NONE) =>
            {
                hearing.finish(true);
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
            hearing.finish(false);
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
            Spoken::Listening => {
                hearing.model = Model::Listening;
                hearing.since = Some(now);
            },
            Spoken::Level(level) => {
                if hearing.levels.len() == LEVELS {
                    hearing.levels.remove(0);
                }
                hearing.levels.push(level);
            },
            Spoken::Words(words) => hearing.words = words,
            Spoken::Ended(Ok(text)) => {
                return Some(Dictated { into: hearing.into, text, sends: hearing.sends });
            },
            Spoken::Ended(Err(why)) => self.stage = Stage::Wrong(why),
        }
        None
    }

    /// Takes in how the model's fetch goes.
    pub fn fetched(&mut self, fetched: &Fetched) {
        match (&mut self.stage, fetched) {
            (Stage::Fetching(got), Fetched::Got(now)) => *got = *now,
            (Stage::Fetching(_), Fetched::Ended(ended)) => {
                self.stage = Stage::Fetched(ended.clone());
            },
            _ => {},
        }
    }

    /// Whether its spinner turns, and its clock: while it hears you, or
    /// writes what you said.
    pub fn turning(&self) -> bool {
        matches!(self.stage, Stage::Hearing(_))
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
            Stage::Fetched(Ok(())) => {
                let said = "The speech model is in. Hold Alt r and talk, or tap it to talk \
                    with your hands free.";
                let lines = menu::wrap(said, inner, Style::new());
                let keys = vec![(Hint::Alt('r'), "Dictate"), (Hint::AnyOtherKey, "Close")];
                (lines, keys, String::new())
            },
            Stage::Fetched(Err(why)) => {
                wrong(&format!("The speech model did not come: {why}"), inner)
            },
            Stage::Wrong(why) => wrong(why, inner),
            Stage::Hearing(hearing) => hearing.lines(inner, now),
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
    /// Has the model write what you said, and send it then, if `sends`.
    fn finish(&mut self, sends: bool) {
        self.sends = sends;
        self.key = Key::Done;
        self.listening.finish();
    }

    /// Its lines, `inner` wide, at `now`, the keys under them, and what its
    /// rule says on the right: how long the model has heard you, and how
    /// loud the microphone heard you last.
    fn lines(&self, inner: usize, now: (SystemTime, Instant)) -> Shown {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let frame = sidebar::frame(now.0);
        let mut lines = menu::wrap(self.words.trim(), inner, Style::new());
        if self.words.trim().is_empty() {
            lines.clear();
        }
        let doing = match self.model {
            _ if self.key == Key::Done => Some("Writing what you said…"),
            Model::Opening => Some("Opening the microphone…"),
            Model::Loading => Some("Reading the speech model in: go on talking…"),
            Model::Listening if lines.is_empty() => {
                lines.push(Line::styled(" Listening…", dim));
                None
            },
            Model::Listening => None,
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
            Key::Done => vec![(Hint::Esc, "Cancel")],
        };
        let since = |since: Instant| now.1.saturating_duration_since(since);
        let heard = self.since.map_or(Duration::ZERO, since);
        let seconds = heard.as_secs();
        let bars: String = self.levels.iter().map(|level| bar(*level)).collect();
        (lines, keys, format!("● {}:{:02} {bars}", seconds / 60, seconds % 60))
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
        let (listening, stop) = Listening::new();
        (Dictation::hearing(1, Where::Zellij, listening, releases, at), stop)
    }

    fn ended(dictation: &mut Dictation, text: &str) -> Option<Dictated> {
        dictation.spoken(Spoken::Ended(Ok(text.to_owned())), Instant::now())
    }

    fn written(text: &str, sends: bool) -> Dictated {
        Dictated { into: Where::Zellij, text: text.to_owned(), sends }
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
