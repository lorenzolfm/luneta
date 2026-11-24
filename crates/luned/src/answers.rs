use std::fmt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use lune_domain::mux::TerminalKey;

use crate::api::{answer_request, waiting_response, WaitingResponse};
use crate::choices::{Choices, Moves, Seen, CHOICES_MAX, SCREEN_LINES_MAX};
use crate::log;
use crate::mux::own::api::{self as mux, pressed, Named};
use crate::mux::own::server::{Looked, Pressed, Shown, Terminals, PRESSES_MAX};
use crate::mux::Failure;
use crate::transcripts;

pub const ANSWER_BYTES_MAX: usize = 64 * 1024;

const SAID_CHARS_MAX: usize = 4000;

const MOVED_LOOKS_MAX: u32 = 20;

const MOVED_LOOK: Duration = Duration::from_millis(50);

const CHANGED: &str = "the screen changed since it was read: read it again";

#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    shown: Shown,
    asked: Asked,
}

#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Text(String),
    Choice { index: usize, label: String },
    Escape,
}

#[derive(Debug)]
pub enum Runs {
    Claude { transcript: Option<PathBuf> },
    Unread(String),
}

#[derive(Debug)]
pub enum Unanswered {
    Changed,
    Refused(String),
    Unreached(Failure),
}

impl From<Failure> for Unanswered {
    fn from(failure: Failure) -> Self {
        Unanswered::Unreached(failure)
    }
}

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unanswered::Changed => f.write_str(CHANGED),
            Unanswered::Refused(why) => f.write_str(why),
            Unanswered::Unreached(failure) => write!(f, "{failure}"),
        }
    }
}

impl Answer {
    pub fn of(
        shown: Option<u64>,
        answer: Option<answer_request::Answer>,
    ) -> Result<Answer, String> {
        let unshown = || "it names no screen it answers".to_owned();
        let shown = shown.map(Shown::of).ok_or_else(unshown)?;
        let asked = match answer {
            None => return Err("it gives no answer".to_owned()),
            Some(answer_request::Answer::Text(text)) => {
                if text.trim().is_empty() {
                    return Err("the answer's text is blank".to_owned());
                }
                if text.len() > ANSWER_BYTES_MAX {
                    return Err(format!("the answer's text is over {ANSWER_BYTES_MAX} bytes"));
                }
                Asked::Text(text)
            },
            Some(answer_request::Answer::Choice(answer_request::Choice { index, label })) => {
                let index = usize::try_from(index).ok().filter(|index| *index < CHOICES_MAX);
                let past = || format!("the choice is past the {CHOICES_MAX} of any menu");
                let index = index.ok_or_else(past)?;
                if label.trim().is_empty() {
                    return Err("the choice's label is blank".to_owned());
                }
                if label.len() > ANSWER_BYTES_MAX {
                    return Err(format!("the choice's label is over {ANSWER_BYTES_MAX} bytes"));
                }
                Asked::Choice { index, label }
            },
            Some(answer_request::Answer::Escape(())) => Asked::Escape,
        };
        Ok(Answer { shown, asked })
    }
}

pub fn waiting(key: TerminalKey, runs: &Runs) -> Result<WaitingResponse, Failure> {
    use waiting_response::Asks;
    let Looked { text, shown } = Terminals.looked(key)?;
    let screen = tail(&text, SCREEN_LINES_MAX);
    let (asks, said) = match runs {
        Runs::Unread(why) => (Asks::Unread(why.clone()), None),
        Runs::Claude { transcript } => {
            let asks = match Seen::of(&screen) {
                Seen::Words => Asks::Words(()),
                Seen::Menu(menu) => Asks::Menu(waiting_response::Menu {
                    choices: menu.labels().to_vec(),
                    chosen: u32::try_from(menu.at()).expect("a menu's choices are few"),
                }),
                Seen::TooMany(count) => Asks::Unread(too_many(count)),
                Seen::Alone(chosen) => Asks::Unread(alone(&chosen)),
            };
            (asks, transcript.as_deref().and_then(said_in))
        },
    };
    Ok(WaitingResponse { screen, shown: shown.said(), asks: Some(asks), said })
}

pub fn answer(key: TerminalKey, runs: &Runs, answer: Answer) -> Result<(), Unanswered> {
    if let Runs::Unread(why) = runs {
        return Err(Unanswered::Refused(why.clone()));
    }
    let Answer { shown, asked } = answer;
    let looked = Terminals.looked(key)?;
    if looked.shown != shown {
        return Err(Unanswered::Changed);
    }
    match (asked, Seen::of(&tail(&looked.text, SCREEN_LINES_MAX))) {
        (Asked::Escape, _) => press(key, shown, vec![named_key(Named::Escape)]),
        (Asked::Text(_) | Asked::Choice { .. }, Seen::TooMany(count)) => {
            Err(Unanswered::Refused(too_many(count)))
        },
        (Asked::Text(_) | Asked::Choice { .. }, Seen::Alone(chosen)) => {
            Err(Unanswered::Refused(alone(&chosen)))
        },
        (Asked::Text(_), Seen::Menu(menu)) => {
            let count = menu.labels().len();
            let why = format!("the screen shows {count} choices: answer with one of them");
            Err(Unanswered::Refused(why))
        },
        (Asked::Text(text), Seen::Words) => {
            let pasted = pressed::Event::Paste(mux::Paste { text });
            press(key, shown, vec![pasted, named_key(Named::Enter)])
        },
        (Asked::Choice { .. }, Seen::Words) => {
            Err(Unanswered::Refused("the screen shows no choices".to_owned()))
        },
        (Asked::Choice { index, label }, Seen::Menu(menu)) => {
            let moves = match menu.moves_to(index, &label).map_err(Unanswered::Refused)? {
                Moves::Up(steps) => vec![named_key(Named::Up); steps],
                Moves::Down(steps) => vec![named_key(Named::Down); steps],
                Moves::Stay => return press(key, shown, vec![named_key(Named::Enter)]),
            };
            assert!(!moves.is_empty() && moves.len() <= PRESSES_MAX, "{} moves", moves.len());
            press(key, shown, moves)?;
            entered_at(key, &menu, index)
        },
    }
}

fn press(key: TerminalKey, shown: Shown, events: Vec<pressed::Event>) -> Result<(), Unanswered> {
    match Terminals.pressed(key, shown, events)? {
        Pressed::Done => Ok(()),
        Pressed::Changed => Err(Unanswered::Changed),
    }
}

fn entered_at(key: TerminalKey, asked: &Choices, index: usize) -> Result<(), Unanswered> {
    let label = &asked.labels()[index];
    for _ in 0..MOVED_LOOKS_MAX {
        let Looked { text, shown } = Terminals.looked(key)?;
        let there = match Seen::of(&tail(&text, SCREEN_LINES_MAX)) {
            Seen::Menu(menu) => menu.at() == index && menu.asks_as(asked),
            Seen::Words | Seen::TooMany(_) | Seen::Alone(_) => false,
        };
        if there {
            match Terminals.pressed(key, shown, vec![named_key(Named::Enter)])? {
                Pressed::Done => return Ok(()),
                Pressed::Changed => {},
            }
        }
        thread::sleep(MOVED_LOOK);
    }
    let number = index + 1;
    let why = format!("the cursor did not reach choice {number}, {label:?}, so Enter was not pressed");
    Err(Unanswered::Refused(why))
}

fn too_many(count: usize) -> String {
    assert!(count > CHOICES_MAX, "{count} choices are read");
    format!("the screen shows {count} choices, over the {CHOICES_MAX} luned reads")
}

fn alone(chosen: &str) -> String {
    format!("the screen's cursor is on {chosen:?}, with no choices luned reads beside it")
}

fn tail(text: &str, lines_max: usize) -> String {
    let shown: Vec<&str> = text.trim_end().lines().collect();
    let kept = shown[shown.len().saturating_sub(lines_max)..].join("\n");
    assert!(kept.lines().count() <= lines_max, "{} lines kept", kept.lines().count());
    kept
}

fn said_in(transcript: &Path) -> Option<String> {
    match transcripts::last_said(transcript, SAID_CHARS_MAX) {
        Ok(said) => said,
        Err(error) => {
            log::say(&format!("{} is unread, so no last words: {error}", transcript.display()));
            None
        },
    }
}

fn named_key(named: Named) -> pressed::Event {
    pressed::Event::Key(mux::Key {
        press: mux::Press::Down.into(),
        modifiers: Some(mux::Modifiers::default()),
        key: Some(mux::key::Key::Named(named.into())),
        text: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_is_its_last_lines_with_no_blank_end() {
        let text = format!("{}\n\n\n", (1..=50).map(|n| n.to_string()).collect::<Vec<_>>().join("\n"));
        let screen = tail(&text, SCREEN_LINES_MAX);
        assert_eq!(screen.lines().count(), SCREEN_LINES_MAX);
        assert!(screen.starts_with("11\n") && screen.ends_with("\n50"), "{screen}");
    }

    #[test]
    fn an_answer_is_read_from_what_was_asked() {
        use answer_request::Answer as Said;
        let asked = |asked: Asked| Ok(Answer { shown: Shown::of(7), asked });
        let text = |text: &str| Answer::of(Some(7), Some(Said::Text(text.to_owned())));
        assert_eq!(text("yes, go on"), asked(Asked::Text("yes, go on".to_owned())));
        assert_eq!(text(" \n "), Err("the answer's text is blank".to_owned()));
        let long = "x".repeat(ANSWER_BYTES_MAX + 1);
        assert_eq!(text(&long), Err(format!("the answer's text is over {ANSWER_BYTES_MAX} bytes")));
        assert_eq!(Answer::of(Some(7), Some(Said::Escape(()))), asked(Asked::Escape));
        assert_eq!(Answer::of(Some(7), None), Err("it gives no answer".to_owned()));
        let unshown = Answer::of(None, Some(Said::Escape(())));
        assert_eq!(unshown, Err("it names no screen it answers".to_owned()));
        let chose = |index: u32, label: &str| {
            let choice = answer_request::Choice { index, label: label.to_owned() };
            Answer::of(Some(7), Some(Said::Choice(choice)))
        };
        let no = Asked::Choice { index: 2, label: "No".to_owned() };
        assert_eq!(chose(2, "No"), asked(no));
        assert_eq!(chose(16, "No"), Err("the choice is past the 16 of any menu".to_owned()));
        assert_eq!(chose(1, " "), Err("the choice's label is blank".to_owned()));
        let over = format!("the choice's label is over {ANSWER_BYTES_MAX} bytes");
        assert_eq!(chose(1, &long), Err(over));
    }
}
