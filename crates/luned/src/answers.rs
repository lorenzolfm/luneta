use std::fmt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::api::{answer_request, WaitingResponse};
use crate::choices::{Choices, Moves, CHOICES_MAX, SCREEN_LINES_MAX};
use crate::log;
use crate::mux::own::api::{self as mux, typed, Dumped, Named};
use crate::mux::own::server::{Terminals, PRESSES_MAX};
use crate::mux::own::TerminalKey;
use crate::mux::Failure;
use crate::transcripts;

pub const ANSWER_BYTES_MAX: usize = 64 * 1024;

const SAID_CHARS_MAX: usize = 4000;

const MOVED_LOOKS_MAX: u32 = 20;

const MOVED_LOOK: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Choice { index: usize, label: String },
    Escape,
}

#[derive(Debug)]
pub enum Unanswered {
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
            Unanswered::Refused(why) => f.write_str(why),
            Unanswered::Unreached(failure) => write!(f, "{failure}"),
        }
    }
}

impl Answer {
    pub fn of(answer: Option<answer_request::Answer>) -> Result<Answer, String> {
        match answer {
            None => Err("it gives no answer".to_owned()),
            Some(answer_request::Answer::Text(text)) => {
                if text.trim().is_empty() {
                    return Err("the answer's text is blank".to_owned());
                }
                if text.len() > ANSWER_BYTES_MAX {
                    return Err(format!("the answer's text is over {ANSWER_BYTES_MAX} bytes"));
                }
                Ok(Answer::Text(text))
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
                Ok(Answer::Choice { index, label })
            },
            Some(answer_request::Answer::Escape(())) => Ok(Answer::Escape),
        }
    }
}

pub fn waiting(key: TerminalKey, transcript: Option<&Path>) -> Result<WaitingResponse, Failure> {
    let screen = screen(key)?;
    assert!(screen.lines().count() <= SCREEN_LINES_MAX, "the screen is cut to its last lines");
    let menu = Choices::of(&screen);
    let chosen = menu.as_ref().map(|menu| u32::try_from(menu.at()).expect("a menu's choices are few"));
    let choices = menu.map_or_else(Vec::new, |menu| menu.labels().to_vec());
    match chosen {
        Some(chosen) => {
            let chosen = usize::try_from(chosen).expect("a u32 fits a usize");
            assert!(chosen < choices.len(), "the cursor is on a choice");
        },
        None => assert!(choices.is_empty(), "choices come with the cursor's place among them"),
    }
    let said = transcript.and_then(said_in);
    Ok(WaitingResponse { screen, choices, chosen, said })
}

pub fn answer(key: TerminalKey, answer: Answer) -> Result<(), Unanswered> {
    match answer {
        Answer::Escape => Ok(Terminals.pressed(key, vec![named_key(Named::Escape)])?),
        Answer::Text(text) => {
            assert!(text.len() <= ANSWER_BYTES_MAX, "the text was read as an answer");
            if let Some(menu) = Choices::of(&screen(key)?) {
                let shown = menu.labels().len();
                let why = format!("the screen shows {shown} choices now: answer with one of them");
                return Err(Unanswered::Refused(why));
            }
            let pasted = typed::Event::Paste(mux::Paste { text });
            Ok(Terminals.pressed(key, vec![pasted, named_key(Named::Enter)])?)
        },
        Answer::Choice { index, label } => {
            assert!(index < CHOICES_MAX, "the choice was read as an answer");
            let Some(menu) = Choices::of(&screen(key)?) else {
                return Err(Unanswered::Refused("the screen shows no choices now".to_owned()));
            };
            let moves = menu.moves_to(index, &label).map_err(Unanswered::Refused)?;
            let pressed = match moves {
                Moves::Up(steps) => vec![named_key(Named::Up); steps],
                Moves::Down(steps) => vec![named_key(Named::Down); steps],
                Moves::Stay => Vec::new(),
            };
            assert!(pressed.len() <= PRESSES_MAX, "{} moves", pressed.len());
            if !pressed.is_empty() {
                Terminals.pressed(key, pressed)?;
                moved_to(key, index, &label)?;
            }
            Ok(Terminals.pressed(key, vec![named_key(Named::Enter)])?)
        },
    }
}

fn moved_to(key: TerminalKey, index: usize, label: &str) -> Result<(), Unanswered> {
    for _ in 0..MOVED_LOOKS_MAX {
        let menu = Choices::of(&screen(key)?);
        if menu.is_some_and(|menu| menu.moves_to(index, label) == Ok(Moves::Stay)) {
            return Ok(());
        }
        thread::sleep(MOVED_LOOK);
    }
    let number = index + 1;
    let why = format!("the cursor did not reach choice {number}, {label:?}, so Enter was not pressed");
    Err(Unanswered::Refused(why))
}

fn screen(key: TerminalKey) -> Result<String, Failure> {
    Ok(tail(&Terminals.dumped(key, Dumped::Shown)?, SCREEN_LINES_MAX))
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

fn named_key(named: Named) -> typed::Event {
    typed::Event::Key(mux::Key {
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
        use answer_request::Answer as Asked;
        let text = |text: &str| Answer::of(Some(Asked::Text(text.to_owned())));
        assert_eq!(text("yes, go on"), Ok(Answer::Text("yes, go on".to_owned())));
        assert_eq!(text(" \n "), Err("the answer's text is blank".to_owned()));
        let long = "x".repeat(ANSWER_BYTES_MAX + 1);
        assert_eq!(text(&long), Err(format!("the answer's text is over {ANSWER_BYTES_MAX} bytes")));
        assert_eq!(Answer::of(Some(Asked::Escape(()))), Ok(Answer::Escape));
        assert_eq!(Answer::of(None), Err("it gives no answer".to_owned()));
        let chose = |index: u32, label: &str| {
            Answer::of(Some(Asked::Choice(answer_request::Choice { index, label: label.to_owned() })))
        };
        assert_eq!(chose(2, "No"), Ok(Answer::Choice { index: 2, label: "No".to_owned() }));
        assert_eq!(chose(16, "No"), Err("the choice is past the 16 of any menu".to_owned()));
        assert_eq!(chose(1, " "), Err("the choice's label is blank".to_owned()));
        let over = format!("the choice's label is over {ANSWER_BYTES_MAX} bytes");
        assert_eq!(chose(1, &long), Err(over));
    }
}
