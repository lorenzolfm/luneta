//! What luneta's own terminal answers of itself: whether it speaks the kitty
//! keyboard protocol, which luneta asks as it starts, and the colours it draws
//! in where a program names none, which luneta answers zellij with when zellij
//! asks its terminal for them. luneta asks for the colours again each time its
//! terminal says it turned dark or light. `input` hears the answers in among
//! the keys, whenever they come.

use std::fmt;
use std::io::{self, Write as _};
use std::str;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

use crate::host::Inbox;

/// What luneta asks as it starts: the foreground and background colours (OSC
/// 10 and 11), the kitty keyboard protocol's flags, and last what the device
/// is, which every terminal answers. A terminal answers in the order it is
/// asked, so that once it says what it is, all it was going to answer has
/// come.
const QUESTIONS: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b[c";

/// What luneta asks when its terminal turns dark or light: its colours, and
/// what it is, to know when they are in.
pub const TURNED: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";

/// The longest luneta waits for the answers as it starts, for a terminal that
/// answers nothing, not even what it is. A colour that comes after is heard
/// all the same, too late for zellij's first question; whether it speaks the
/// kitty keyboard protocol, too late to count.
const WAIT_MAX: Duration = Duration::from_secs(1);

/// An answer of luneta's terminal, or what it says of itself unasked once
/// asked to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The colour it draws text in where a program names none (OSC 10).
    Foreground(Rgb),
    /// The colour it draws the background in where a program names none (OSC
    /// 11).
    Background(Rgb),
    /// It speaks the kitty keyboard protocol: it said which of its flags are
    /// on.
    Kitty,
    /// What it is, the last answer of those asked of it together.
    Device,
    /// It turned dark or light, as it says unasked while luneta asks it to
    /// (mode 2031).
    Turned(Theme),
}

/// Whether luneta's terminal draws dark or light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    /// How a terminal says it turned to this: `CSI ? 997 ; 1 n` for dark, `2`
    /// for light, as zellij hears it from its own.
    pub fn report(self) -> &'static [u8] {
        match self {
            Theme::Dark => b"\x1b[?997;1n",
            Theme::Light => b"\x1b[?997;2n",
        }
    }
}

/// What luneta's terminal said of itself as luneta started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    /// Whether it speaks the kitty keyboard protocol.
    pub kitty: bool,
    pub colours: Colours,
}

/// The colours luneta's terminal draws a program's text and background in
/// where the program names none, those of them it said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colours {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
}

impl Colours {
    /// Takes in what luneta's terminal said of one of them, where `answer` is
    /// one.
    pub fn hear(&mut self, answer: Answer) {
        match answer {
            Answer::Foreground(colour) => self.foreground = Some(colour),
            Answer::Background(colour) => self.background = Some(colour),
            Answer::Kitty | Answer::Device | Answer::Turned(_) => {},
        }
    }

    /// The answer to a program that asks for the colour alacritty numbers
    /// `index`, as `spell` spells it: none for one luneta's terminal did not
    /// say, or for one of the rest, as the sixteen and the cursor's.
    pub fn answer(self, index: usize, spell: &dyn Fn(Rgb) -> String) -> Option<String> {
        let colour = if index == NamedColor::Foreground as usize {
            self.foreground
        } else if index == NamedColor::Background as usize {
            self.background
        } else {
            None
        };
        colour.map(spell)
    }
}

/// Asks luneta's terminal what it is, and waits `WAIT_MAX` at the most for the
/// answers, which `inbox` brings in among what else comes, as keys typed as
/// luneta starts: those wait in `inbox` for the loop, in the order they came.
/// What the terminal did not answer by then, it does not speak or did not
/// say, as for a terminal that cannot be asked.
pub fn ask(inbox: &mut Inbox) -> Answers {
    let mut output = io::stdout().lock();
    if output.write_all(QUESTIONS).and_then(|()| output.flush()).is_err() {
        return Answers::default();
    }
    drop(output);
    hear(inbox, Instant::now() + WAIT_MAX)
}

/// The answers `inbox` brings until the terminal says what it is, or until
/// `deadline`.
fn hear(inbox: &mut Inbox, deadline: Instant) -> Answers {
    let mut answers = Answers::default();
    while let Some(answer) = inbox.answer(deadline) {
        match answer {
            Answer::Device => break,
            Answer::Kitty => answers.kitty = true,
            Answer::Foreground(_) | Answer::Background(_) => answers.colours.hear(answer),
            // The colours it answers with are those it turned to.
            Answer::Turned(_) => {},
        }
    }
    answers
}

/// Asks luneta's terminal to say when it turns dark or light (mode 2031), as
/// `Answer::Turned`.
pub struct EnableThemeChange;

/// Asks luneta's terminal to stop saying when it turns dark or light.
pub struct DisableThemeChange;

impl crossterm::Command for EnableThemeChange {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?2031h")
    }
}

impl crossterm::Command for DisableThemeChange {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?2031l")
    }
}

/// The colour X11 spells `rgb:R/G/B`, as terminals answer with: each of red,
/// green and blue in one to four hex digits, of which `f`, `ff`, `fff` and
/// `ffff` are all the most there is.
pub fn rgb(spelled: &[u8]) -> Option<Rgb> {
    let channels = str::from_utf8(spelled).ok()?.strip_prefix("rgb:")?;
    let channels: Vec<u8> = channels.split('/').map(channel).collect::<Option<_>>()?;
    let [r, g, b] = channels[..] else {
        return None;
    };
    Some(Rgb { r, g, b })
}

/// A channel of one to four hex digits, in eight bits.
fn channel(digits: &str) -> Option<u8> {
    if !(1..=4).contains(&digits.len()) || !digits.bytes().all(|digit| digit.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let most = (1 << (4 * digits.len())) - 1;
    u8::try_from(value * 255 / most).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::host::Event;

    fn typed(c: char) -> Event {
        Event::Input(Input::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
    }

    fn is_typed(event: Option<&Event>, c: char) -> bool {
        matches!(event, Some(Event::Input(Input::Key(key))) if key.code == KeyCode::Char(c))
    }

    /// Keys typed as luneta starts wait for the loop, in the order they came,
    /// around the answers, and what comes after the terminal says what it is
    /// is the loop's too.
    #[test]
    fn the_answers_are_heard_until_what_the_terminal_is_and_the_keys_wait() {
        let (events, inbox) = mpsc::channel();
        let background = Rgb { r: 0x1e, g: 0x1e, b: 0x2e };
        for event in [
            typed('l'),
            Event::Answered(Answer::Background(background)),
            typed('s'),
            Event::Answered(Answer::Kitty),
            Event::Answered(Answer::Device),
            Event::Answered(Answer::Foreground(background)),
        ] {
            events.send(event).unwrap();
        }
        let mut inbox = Inbox::new(inbox);
        let heard = hear(&mut inbox, Instant::now() + Duration::from_secs(10));
        let mut colours = Colours::default();
        colours.hear(Answer::Background(background));
        assert_eq!(heard, Answers { kitty: true, colours });
        assert!(is_typed(inbox.try_next().as_ref(), 'l'));
        assert!(is_typed(inbox.try_next().as_ref(), 's'));
        assert!(matches!(inbox.try_next(), Some(Event::Answered(Answer::Foreground(_)))));
        assert!(inbox.try_next().is_none());
        // A terminal that answers nothing is done with by the deadline.
        assert_eq!(hear(&mut inbox, Instant::now()), Answers::default());
    }

    #[test]
    fn colours_are_read_in_one_to_four_digits_and_nothing_else() {
        let spelled = |colour: &str| rgb(colour.as_bytes());
        assert_eq!(spelled("rgb:f/8/0"), Some(Rgb { r: 0xff, g: 0x88, b: 0 }));
        assert_eq!(spelled("rgb:fff/800/000"), Some(Rgb { r: 0xff, g: 0x7f, b: 0 }));
        assert_eq!(spelled("rgb:1e1e/8080/abcd"), Some(Rgb { r: 0x1e, g: 0x80, b: 0xab }));
        for wrong in ["rgb:ff/ff", "rgb:ff/ff/ff/ff", "rgb:fffff/0/0", "rgb:+f/0/0", "#ffffff"] {
            assert_eq!(spelled(wrong), None, "{wrong}");
        }
    }
}
