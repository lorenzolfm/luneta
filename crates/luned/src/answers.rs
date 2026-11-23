use std::path::Path;

use crate::api::{answer_request, WaitingResponse};
use crate::choices::{Choices, Moves};
use crate::mux::own::api::{self as mux, typed, Dumped, Named};
use crate::mux::own::server::Terminals;
use crate::mux::own::TerminalKey;
use crate::transcripts;

const SCREEN_LINES: usize = 40;

pub const TEXT_BYTES_MAX: usize = 64 * 1024;

const SAID_CHARS: usize = 4000;

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Choice { index: usize, label: String },
    Escape,
}

impl Answer {
    pub fn of(answer: Option<answer_request::Answer>) -> Result<Answer, String> {
        match answer {
            None => Err("it gives no answer".to_owned()),
            Some(answer_request::Answer::Text(text)) if text.trim().is_empty() => {
                Err("the answer's text is blank".to_owned())
            },
            Some(answer_request::Answer::Text(text)) if text.len() > TEXT_BYTES_MAX => {
                Err(format!("the answer's text is over {TEXT_BYTES_MAX} bytes"))
            },
            Some(answer_request::Answer::Text(text)) => Ok(Answer::Text(text)),
            Some(answer_request::Answer::Choice(answer_request::Choice { index, label })) => {
                let index = usize::try_from(index).map_err(|_| "the choice is past any screen")?;
                Ok(Answer::Choice { index, label })
            },
            Some(answer_request::Answer::Escape(())) => Ok(Answer::Escape),
        }
    }
}

pub fn waiting(key: TerminalKey, transcript: Option<&Path>) -> Result<WaitingResponse, String> {
    let screen = screen(key)?;
    let (choices, chosen) = match Choices::of(&screen) {
        Some(choices) => {
            let chosen = u32::try_from(choices.at()).expect("a screen's choices are few");
            (choices.labels().to_vec(), chosen)
        },
        None => (Vec::new(), 0),
    };
    let said = transcript.and_then(|path| transcripts::last_said(path, SAID_CHARS).ok().flatten());
    Ok(WaitingResponse { screen, choices, chosen, said })
}

pub fn answer(key: TerminalKey, answer: Answer) -> Result<(), String> {
    let pressed = match answer {
        Answer::Text(text) => vec![typed::Event::Paste(mux::Paste { text }), named(Named::Enter)],
        Answer::Escape => vec![named(Named::Escape)],
        Answer::Choice { index, label } => {
            let Some(choices) = Choices::of(&screen(key)?) else {
                return Err("the screen shows no choices now".to_owned());
            };
            let (step, steps) = match choices.moves_to(index, &label)? {
                Moves::Up(steps) => (Named::Up, steps),
                Moves::Down(steps) => (Named::Down, steps),
                Moves::Stay => (Named::Enter, 0),
            };
            let mut pressed = vec![named(step); steps];
            pressed.push(named(Named::Enter));
            pressed
        },
    };
    Ok(Terminals.pressed(key, pressed)?)
}

fn screen(key: TerminalKey) -> Result<String, String> {
    Ok(tail(&Terminals.dumped(key, Dumped::Text)?, SCREEN_LINES))
}

fn tail(text: &str, lines: usize) -> String {
    let shown: Vec<&str> = text.trim_end().lines().collect();
    shown[shown.len().saturating_sub(lines)..].join("\n")
}

fn named(named: Named) -> typed::Event {
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
        let screen = tail(&text, SCREEN_LINES);
        assert_eq!(screen.lines().count(), SCREEN_LINES);
        assert!(screen.starts_with("11\n") && screen.ends_with("\n50"), "{screen}");
    }

    #[test]
    fn an_answer_is_read_from_what_was_asked() {
        use answer_request::Answer as Asked;
        let text = |text: &str| Answer::of(Some(Asked::Text(text.to_owned())));
        assert_eq!(text("yes, go on"), Ok(Answer::Text("yes, go on".to_owned())));
        assert_eq!(text(" \n "), Err("the answer's text is blank".to_owned()));
        let long = "x".repeat(TEXT_BYTES_MAX + 1);
        assert_eq!(text(&long), Err(format!("the answer's text is over {TEXT_BYTES_MAX} bytes")));
        let choice = answer_request::Choice { index: 2, label: "No".to_owned() };
        let choice = Answer::of(Some(Asked::Choice(choice)));
        assert_eq!(choice, Ok(Answer::Choice { index: 2, label: "No".to_owned() }));
        assert_eq!(Answer::of(Some(Asked::Escape(()))), Ok(Answer::Escape));
        assert_eq!(Answer::of(None), Err("it gives no answer".to_owned()));
    }
}
