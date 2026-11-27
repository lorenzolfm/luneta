use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::brief;
use crate::cadence::Cadence;
use crate::clock::Moment;
use crate::menu::{self, Hint};
use crate::models::Fetched;
use crate::named::{self, Written};
use crate::sidebar::{self, ACCENT};
use crate::skills::{Name, Skill, Skills};
use crate::speech::{self, Listening, Spoken};

pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('r'), KeyModifiers::ALT);

const TAP: Duration = Duration::from_millis(300);

const REPEATS: Duration = Duration::from_millis(150);

const TITLE: &str = "Dictate";

const WRITING: &str = "Writing what you said…";

const ONLY_FIRST: &str = "Runs only if named first:";

const LEVELS: usize = 8;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[derive(Debug, PartialEq, Eq)]
pub struct Dictated {
    pub said: String,
    pub written: Written,
    pub sends: bool,
}

impl Dictated {
    fn of(said: String, skills: &Skills, ending: &Ending) -> Self {
        let written = written(&said, skills);
        let sends = ending.sends(&written);
        Dictated {
            said,
            written,
            sends,
        }
    }

    pub fn moved(self) -> Self {
        if self.written.command.is_none() {
            return self;
        }
        let written = Written::said(&self.said);
        Dictated {
            said: self.said,
            written,
            sends: false,
        }
    }
}

#[derive(Debug)]
pub struct Repeats {
    key: (KeyCode, KeyModifiers),
    last: Option<Instant>,
}

impl Repeats {
    pub fn of(key: (KeyCode, KeyModifiers)) -> Repeats {
        Repeats { key, last: None }
    }

    pub fn read(&mut self, key: KeyEvent, now: Instant) -> KeyEvent {
        if (key.code, key.modifiers) != self.key || key.kind == KeyEventKind::Release {
            return key;
        }
        let soon = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < REPEATS);
        self.last = Some(now);
        match key.kind {
            KeyEventKind::Press if soon => KeyEvent {
                kind: KeyEventKind::Repeat,
                ..key
            },
            _ => key,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    Changed,
    Close,
    Dictate,
}

#[derive(Debug)]
pub struct Dictation {
    number: u64,
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    Fetching(u64),
    Fetched,
    Hearing(Hearing),
    Naming { said: String, ending: Ending },
    Wrong(String),
}

#[derive(Debug)]
struct Hearing {
    key: Key,
    model: Model,
    levels: Vec<f32>,
    words: String,
    skills: Skills,
    listening: Listening,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Down(Instant),
    Tapped,
    Done(Ending),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ending {
    Writes,
    Sends { showed: Option<Name> },
}

impl Ending {
    fn sends(&self, written: &Written) -> bool {
        matches!(self, Ending::Sends { showed } if *showed == written.command)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Opening,
    Loading,
    Listening(Instant),
}

impl Dictation {
    pub fn hearing(
        number: u64,
        listening: Listening,
        skills: Skills,
        releases: bool,
        now: Instant,
    ) -> Self {
        let key = if releases {
            Key::Down(now)
        } else {
            Key::Tapped
        };
        let hearing = Hearing {
            key,
            model: Model::Opening,
            levels: Vec::new(),
            words: String::new(),
            skills,
            listening,
        };
        Dictation {
            number,
            stage: Stage::Hearing(hearing),
        }
    }

    pub fn fetching(number: u64, got: u64) -> Self {
        Dictation {
            number,
            stage: Stage::Fetching(got),
        }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn key(&mut self, key: &KeyEvent, now: Instant) -> Keyed {
        let pressed = key.kind == KeyEventKind::Press;
        let chord = (key.code, key.modifiers);
        let closes = chord == (KeyCode::Esc, KeyModifiers::NONE)
            || chord == (KeyCode::Char('c'), KeyModifiers::CONTROL);
        let hearing = match &mut self.stage {
            Stage::Hearing(hearing) => hearing,
            Stage::Naming { .. } => {
                return if pressed && closes {
                    Keyed::Close
                } else {
                    Keyed::Nothing
                };
            }
            Stage::Fetching(_) | Stage::Fetched | Stage::Wrong(_) => {
                return match (pressed, chord == KEY) {
                    (true, true) => Keyed::Dictate,
                    (true, false) => Keyed::Close,
                    (false, _) => Keyed::Nothing,
                };
            }
        };
        if pressed && closes {
            return Keyed::Close;
        }
        match (&hearing.key, key.kind) {
            (Key::Down(down), KeyEventKind::Release) if key.code == KEY.0 => {
                if now.saturating_duration_since(*down) < TAP {
                    hearing.key = Key::Tapped;
                } else {
                    hearing.finish(Ending::Writes);
                }
                Keyed::Changed
            }
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press) if chord == KEY => {
                hearing.finish(Ending::Writes);
                Keyed::Changed
            }
            (Key::Down(_) | Key::Tapped, KeyEventKind::Press)
                if chord == (KeyCode::Enter, KeyModifiers::NONE) =>
            {
                let showed = written(hearing.words.trim(), &hearing.skills).command;
                hearing.finish(Ending::Sends { showed });
                Keyed::Changed
            }
            _ => Keyed::Nothing,
        }
    }

    #[must_use]
    pub fn unfocused(&mut self) -> bool {
        if let Stage::Hearing(hearing) = &mut self.stage
            && matches!(hearing.key, Key::Down(_))
        {
            hearing.finish(Ending::Writes);
            return true;
        }
        false
    }

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
            }
            Spoken::Words(words) => hearing.words = words,
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
            }
            Spoken::Ended(Err(why)) => self.stage = Stage::Wrong(why),
        }
        None
    }

    pub fn found(&mut self, skills: Result<Vec<Skill>, String>) -> Option<Dictated> {
        let skills = match skills {
            Ok(skills) => Skills::Found(skills),
            Err(why) => Skills::Failed(why),
        };
        match &mut self.stage {
            Stage::Hearing(hearing) => {
                hearing.skills = skills;
                None
            }
            Stage::Naming { said, ending } => Some(Dictated::of(said.clone(), &skills, ending)),
            Stage::Fetching(_) | Stage::Fetched | Stage::Wrong(_) => None,
        }
    }

    pub fn fetched(&mut self, fetched: &Fetched) {
        match (&mut self.stage, fetched) {
            (Stage::Fetching(got), Fetched::Got(now)) => *got = *now,
            (Stage::Fetching(_), Fetched::Ended(Ok(()))) => self.stage = Stage::Fetched,
            (Stage::Fetching(_), Fetched::Ended(Err(why))) => {
                self.stage = Stage::Wrong(format!("The speech model did not come: {why}"));
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub fn turning(&self) -> bool {
        matches!(self.stage, Stage::Hearing(_) | Stage::Naming { .. })
    }

    fn listening_since(&self) -> Option<Instant> {
        let Stage::Hearing(hearing) = &self.stage else {
            return None;
        };
        let Model::Listening(since) = hearing.model else {
            return None;
        };
        Some(since)
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: Moment) -> Cadence {
        let mut cadence = Cadence::default();
        let Some((inner, room)) = brief::room(over) else {
            return cadence;
        };
        let ((mut lines, keys, right), spinner) = match &self.stage {
            Stage::Fetching(got) => (fetching(*got, inner), None),
            Stage::Fetched => {
                let said = "The speech model is in. Hold Alt r and talk, or tap it to talk \
                    with your hands free.";
                let lines = menu::wrap(said, inner, Style::new());
                let keys = vec![(Hint::Alt('r'), "Dictate"), (Hint::AnyOtherKey, "Close")];
                ((lines, keys, String::new()), None)
            }
            Stage::Wrong(why) => (wrong(why, inner), None),
            Stage::Hearing(hearing) => hearing.lines(inner, now),
            Stage::Naming { said, .. } => {
                let mut lines = menu::wrap(said.trim(), inner, Style::new());
                lines.push(Line::default());
                let dim = Style::new().add_modifier(Modifier::DIM);
                let start = lines.len();
                lines.extend(menu::waiting(
                    WRITING,
                    inner,
                    dim,
                    sidebar::frame(now.system),
                ));
                let spinner = (lines.len() > start).then_some(start);
                ((lines, vec![(Hint::Esc, "Cancel")], String::new()), spinner)
            }
        };
        let footer = [Line::default(), menu::keys_line(inner, &keys)];
        let lines_room = room.saturating_sub(footer.len());
        let drained = lines.len().saturating_sub(lines_room);
        if drained > 0 {
            lines.drain(..drained);
        }
        lines.extend(footer);
        let width = u16::try_from(inner + 2)
            .unwrap_or(over.width)
            .min(over.width);
        let height = u16::try_from(lines.len() + 2)
            .unwrap_or(over.height)
            .min(over.height);
        let x = over.x + (over.width - width) / 2;
        let y = (over.bottom().saturating_sub(height + 1)).max(over.y);
        let area = Rect::new(x, y, width, height);
        menu::boxed(buffer, area, &menu::rule(TITLE, &right, inner), &lines);
        if inner > 0 && lines_room > 0 && spinner.is_some_and(|at| at >= drained) {
            cadence.spin();
        }
        if menu::rule_fits(TITLE, &right, inner)
            && let Some(since) = self.listening_since()
        {
            cadence.every_second(since, now.steady);
        }
        cadence
    }
}

impl Hearing {
    fn finish(&mut self, ending: Ending) {
        self.key = Key::Done(ending);
        self.listening.finish();
    }

    fn lines(&self, inner: usize, now: Moment) -> (Shown, Option<usize>) {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let frame = sidebar::frame(now.system);
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
            }
            Model::Listening(_) => None,
        };
        let mut spinner = None;
        if let Some(doing) = doing {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            let start = lines.len();
            lines.extend(menu::waiting(doing, inner, dim, frame));
            if lines.len() > start {
                spinner = Some(start);
            }
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
            Model::Listening(since) => now.steady.saturating_duration_since(since),
            Model::Opening | Model::Loading => Duration::ZERO,
        };
        let seconds = heard.as_secs();
        let bars: String = self.levels.iter().map(|level| bar(*level)).collect();
        (
            (
                lines,
                keys,
                format!("● {}:{:02} {bars}", seconds / 60, seconds % 60),
            ),
            spinner,
        )
    }
}

fn written(said: &str, skills: &Skills) -> Written {
    match skills {
        Skills::Found(skills) => named::written(said, skills),
        Skills::Finding | Skills::Failed(_) => Written::said(said),
    }
}

type Shown = (Vec<Line<'static>>, Vec<(Hint, &'static str)>, String);

fn fetching(got: u64, inner: usize) -> Shown {
    let said = format!(
        "lune fetches its speech model, once: {} of {} MB, from Hugging Face. It goes on \
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

fn wrong(why: &str, inner: usize) -> Shown {
    let lines = brief::paragraphs(why, inner, Style::new().fg(sidebar::ERROR));
    (
        lines,
        vec![(Hint::Alt('r'), "Try again"), (Hint::AnyOtherKey, "Close")],
        String::new(),
    )
}

fn progress(got: u64, of: u64, inner: usize) -> Line<'static> {
    let room = inner.saturating_sub(2);
    let full = usize::try_from(got.min(of) * room as u64 / of.max(1)).unwrap_or(room);
    Line::from(vec![
        Span::raw(" "),
        Span::styled("━".repeat(full), Style::new().fg(ACCENT)),
        Span::styled(
            "━".repeat(room - full),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ])
}

fn bar(level: f32) -> char {
    let at = (level.clamp(0.0, 1.0) * (BARS.len() - 1) as f32).round() as usize;
    BARS[at.min(BARS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, TryRecvError};
    use std::time::SystemTime;

    use super::*;
    use crate::moonlight;
    use crate::skills::Runs;

    fn alt_r(kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(KeyCode::Char('r'), KeyModifiers::ALT, kind)
    }

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn hearing(releases: bool, at: Instant) -> (Dictation, Receiver<()>) {
        hearing_of(Skills::Found(Vec::new()), releases, at)
    }

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
        Dictated {
            said: text.to_owned(),
            written: Written::said(text),
            sends,
        }
    }

    fn runs(said: &str, name: &str, text: &str, sends: bool) -> Dictated {
        let command = Name::new(name.to_owned());
        let written = Written {
            command,
            text: text.to_owned(),
            only_first: Vec::new(),
        };
        Dictated {
            said: said.to_owned(),
            written,
            sends,
        }
    }

    #[test]
    fn a_hold_writes_what_you_said_as_the_key_is_let_go() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        let held = at + Duration::from_millis(100);
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Repeat), held),
            Keyed::Nothing
        );
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        let let_go = KeyEvent::new_with_kind(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(
            dictation.key(&let_go, at + Duration::from_secs(2)),
            Keyed::Changed
        );
        assert_eq!(stop.try_recv(), Ok(()));
        assert_eq!(
            ended(&mut dictation, "Open a tab."),
            Some(written("Open a tab.", false))
        );
    }

    #[test]
    fn a_tap_listens_until_the_key_goes_down_again() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        let tapped = at + Duration::from_millis(150);
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Release), tapped),
            Keyed::Changed
        );
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            dictation.key(&typed, at + Duration::from_secs(1)),
            Keyed::Nothing
        );
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        let again = at + Duration::from_secs(9);
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Press), again),
            Keyed::Changed
        );
        assert_eq!(stop.try_recv(), Ok(()));
    }

    #[test]
    fn where_no_key_is_told_let_go_the_key_taps() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(false, at);
        assert!(!dictation.unfocused());
        assert_eq!(stop.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Press), at),
            Keyed::Changed
        );
        assert_eq!(stop.try_recv(), Ok(()));
    }

    #[test]
    fn the_key_pressed_as_soon_after_as_a_key_repeats_is_held() {
        let at = Instant::now();
        let mut repeats = Repeats::of(KEY);
        let mut read =
            |key: KeyEvent, after: u64| repeats.read(key, at + Duration::from_millis(after)).kind;
        let press = alt_r(KeyEventKind::Press);
        assert_eq!(read(press, 0), KeyEventKind::Press);
        assert_eq!(read(press, 500), KeyEventKind::Press);
        assert_eq!(read(press, 530), KeyEventKind::Repeat);
        assert_eq!(read(alt_r(KeyEventKind::Repeat), 560), KeyEventKind::Repeat);
        assert_eq!(read(press, 590), KeyEventKind::Repeat);
        assert_eq!(
            read(alt_r(KeyEventKind::Release), 600),
            KeyEventKind::Release
        );
        assert_eq!(read(press, 2_000), KeyEventKind::Press);
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(read(typed, 2_010), KeyEventKind::Press);
    }

    #[test]
    fn enter_writes_what_you_said_and_sends_it() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(false, at);
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(dictation.key(&enter, at), Keyed::Changed);
        assert_eq!(stop.try_recv(), Ok(()));
        assert_eq!(
            ended(&mut dictation, "Run the tests."),
            Some(written("Run the tests.", true))
        );
    }

    #[test]
    fn esc_and_ctrl_c_give_the_dictation_up() {
        let at = Instant::now();
        for key in [KeyCode::Esc, KeyCode::Char('c')] {
            let (mut dictation, stop) = hearing(true, at);
            let modifiers = if key == KeyCode::Esc {
                KeyModifiers::NONE
            } else {
                KeyModifiers::CONTROL
            };
            assert_eq!(dictation.key(&pressed(key, modifiers), at), Keyed::Close);
            drop(dictation);
            assert_eq!(stop.try_recv(), Err(TryRecvError::Disconnected));
        }
    }

    #[test]
    fn a_key_held_as_the_window_loses_the_focus_writes_what_you_said() {
        let at = Instant::now();
        let (mut dictation, stop) = hearing(true, at);
        let _ = dictation.unfocused();
        assert_eq!(stop.try_recv(), Ok(()));
    }

    #[test]
    fn what_went_wrong_shows_until_a_key_goes_down() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing(true, at);
        assert_eq!(
            dictation.spoken(Spoken::Ended(Err(speech::UNHEARD.to_owned())), at),
            None
        );
        assert!(!dictation.turning());
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Release), at),
            Keyed::Nothing
        );
        let typed = pressed(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(dictation.key(&typed, at), Keyed::Close);
    }

    #[test]
    fn the_key_on_a_box_that_says_something_dictates() {
        let at = Instant::now();
        let mut dictation = Dictation::fetching(1, 0);
        dictation.fetched(&Fetched::Ended(Ok(())));
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Release), at),
            Keyed::Nothing
        );
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Press), at),
            Keyed::Dictate
        );
    }

    const Z: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

    fn shown(dictation: &Dictation, width: u16, height: u16, now: Instant) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        dictation.draw(
            &mut buffer,
            area,
            Moment {
                system: SystemTime::UNIX_EPOCH,
                steady: now,
            },
        );
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(
                moonlight::ALL.contains(&cell.fg),
                "{:?} in {:?}",
                cell.symbol(),
                cell.fg
            );
        }
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn the_box_shows_what_you_said_as_you_say_it() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing(true, at);
        dictation.spoken(Spoken::Listening, at);
        for level in [0.0, 0.5, 1.0] {
            dictation.spoken(Spoken::Level(level), at);
        }
        let words = "Open a new tab in the lune session and run the tests";
        dictation.spoken(Spoken::Words(words.to_owned()), at);
        assert_eq!(
            shown(&dictation, 60, 9, at + Duration::from_secs(64)),
            [
                Z,
                Z,
                Z,
                "┌─ Dictate ─────────────────────────────────── ● 1:04 ▁▅█ ─┐",
                "│ Open a new tab in the lune session and run the tests     │",
                "│                                                          │",
                "│ <Alt r> - Let go to write, <ESC> - Cancel                │",
                "└──────────────────────────────────────────────────────────┘",
                Z,
            ]
        );
    }

    #[test]
    fn the_box_shows_a_skill_named_as_its_command() {
        let at = Instant::now();
        let named = Skills::Found(skills(&["thermo-nuclear-review"]));
        let (mut dictation, stop) = hearing_of(named, false, at);
        dictation.spoken(Spoken::Listening, at);
        let said = "review this with the thermonuclear review skill";
        dictation.spoken(Spoken::Words(said.to_owned()), at);
        let lines = shown(&dictation, 60, 7, at);
        assert_eq!(
            lines[2],
            "│ /thermo-nuclear-review review this                       │"
        );
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(dictation.key(&enter, at), Keyed::Changed);
        assert_eq!(stop.try_recv(), Ok(()));
        let said = "Review this with the thermonuclear review skill.";
        assert_eq!(
            ended(&mut dictation, said),
            Some(runs(said, "thermo-nuclear-review", "Review this", true))
        );
    }

    #[test]
    fn enter_sends_only_the_command_the_box_showed() {
        let at = Instant::now();
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        for (shown, sent) in [
            ("Run the tests", written("Run all the tests.", true)),
            (
                "slash tiger style",
                runs("Slash tiger style, now.", "tigerstyle", "now.", true),
            ),
            (
                "Review the tiger style",
                runs(
                    "Review the tiger style skill.",
                    "tigerstyle",
                    "Review",
                    false,
                ),
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

    #[test]
    fn a_command_goes_back_to_words_where_the_agent_has_gone() {
        let said = "Slash tiger style, now.";
        assert_eq!(
            runs(said, "tigerstyle", "now.", true).moved(),
            written(said, false)
        );
        assert_eq!(
            written("Run the tests.", true).moved(),
            written("Run the tests.", true)
        );
    }

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
        assert_eq!(
            lines[2],
            "│ /tigerstyle and then the handoff skill                   │"
        );
        assert_eq!(
            lines[4],
            "│ Runs only if named first: /handoff                       │"
        );
    }

    #[test]
    fn what_may_name_a_skill_waits_for_the_finder() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing_of(Skills::Finding, false, at);
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        dictation.key(&enter, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        assert!(dictation.turning());
        let lines = shown(&dictation, 60, 9, at);
        assert_eq!(
            lines[2],
            "│ Slash hand off.                                          │"
        );
        assert!(lines[4].contains("Writing what you said…"), "{lines:?}");
        assert_eq!(dictation.key(&enter, at), Keyed::Nothing);
        assert_eq!(
            dictation.key(&alt_r(KeyEventKind::Press), at),
            Keyed::Nothing
        );
        let found = dictation.found(Ok(skills(&["handoff"])));
        assert_eq!(found, Some(runs("Slash hand off.", "handoff", "", false)));

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        let esc = pressed(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(dictation.key(&esc, at), Keyed::Close);
    }

    #[test]
    fn the_finder_holds_up_only_what_it_can_change() {
        let at = Instant::now();
        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(ended(&mut dictation, "Slash hand off."), None);
        let failed = Err("skills: no answer".to_owned());
        assert_eq!(
            dictation.found(failed),
            Some(written("Slash hand off.", false))
        );

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(
            ended(&mut dictation, "Run the tests."),
            Some(written("Run the tests.", false))
        );

        let (mut dictation, _stop) = hearing_of(Skills::Finding, true, at);
        assert_eq!(dictation.found(Ok(skills(&["handoff"]))), None);
        let said = "Slash hand off.";
        assert_eq!(
            ended(&mut dictation, said),
            Some(runs(said, "handoff", "", false))
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
                "│ lune fetches its speech model, once: 182 of 731 MB, from │",
                "│ Hugging Face. It goes on with this box closed. Dictate   │",
                "│ once it is in.                                           │",
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
