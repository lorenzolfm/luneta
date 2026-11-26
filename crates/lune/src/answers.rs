use std::fmt;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::str;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::Rgb;

use crate::host::Inbox;

const QUESTIONS: &[u8] =
    b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[14t\x1b[c";

pub const TURNED: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";

const WAIT_MAX: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPixels {
    pub width: NonZeroU16,
    pub height: NonZeroU16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPixels {
    pub width: NonZeroU16,
    pub height: NonZeroU16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Foreground(Rgb),
    Background(Rgb),
    Kitty,
    Graphics,
    CellSize(CellPixels),
    WindowPixels(WindowPixels),
    Device,
    Turned(Theme),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    pub kitty: bool,
    pub graphics: bool,
    pub cell: Option<CellPixels>,
    pub window_pixels: Option<WindowPixels>,
    pub colours: Colours,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colours {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
}

impl Colours {
    pub fn hear(&mut self, answer: Answer) {
        match answer {
            Answer::Foreground(colour) => self.foreground = Some(colour),
            Answer::Background(colour) => self.background = Some(colour),
            Answer::Kitty
            | Answer::Graphics
            | Answer::CellSize(_)
            | Answer::WindowPixels(_)
            | Answer::Device
            | Answer::Turned(_) => {},
        }
    }

    pub fn both(self) -> (Option<[u8; 3]>, Option<[u8; 3]>) {
        let rgb = |colour: Rgb| [colour.r, colour.g, colour.b];
        (self.foreground.map(rgb), self.background.map(rgb))
    }
}

pub fn ask(inbox: &mut Inbox) -> Answers {
    let mut output = io::stdout().lock();
    if output.write_all(QUESTIONS).and_then(|()| output.flush()).is_err() {
        return Answers::default();
    }
    drop(output);
    hear(inbox, Instant::now() + WAIT_MAX)
}

fn hear(inbox: &mut Inbox, deadline: Instant) -> Answers {
    let mut answers = Answers::default();
    while let Some(answer) = inbox.answer(deadline) {
        match answer {
            Answer::Device => break,
            Answer::Kitty => answers.kitty = true,
            Answer::Graphics => answers.graphics = true,
            Answer::CellSize(cell) => answers.cell = Some(cell),
            Answer::WindowPixels(pixels) => answers.window_pixels = Some(pixels),
            Answer::Foreground(_) | Answer::Background(_) => answers.colours.hear(answer),
            Answer::Turned(_) => {},
        }
    }
    answers
}

pub struct EnableThemeChange;

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

pub fn rgb(spelled: &[u8]) -> Option<Rgb> {
    let channels = str::from_utf8(spelled).ok()?.strip_prefix("rgb:")?;
    let channels: Vec<u8> = channels.split('/').map(channel).collect::<Option<_>>()?;
    let [r, g, b] = channels[..] else {
        return None;
    };
    Some(Rgb { r, g, b })
}

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

    #[test]
    fn the_answers_are_heard_until_what_the_terminal_is_and_the_keys_wait() {
        let (events, inbox) = mpsc::channel();
        let background = Rgb { r: 0x1e, g: 0x1e, b: 0x2e };
        let cell = CellPixels {
            width: NonZeroU16::new(10).unwrap(),
            height: NonZeroU16::new(20).unwrap(),
        };
        for event in [
            typed('l'),
            Event::Answered(Answer::Background(background)),
            typed('s'),
            Event::Answered(Answer::Kitty),
            Event::Answered(Answer::Graphics),
            Event::Answered(Answer::CellSize(cell)),
            Event::Answered(Answer::Device),
            Event::Answered(Answer::Foreground(background)),
        ] {
            events.send(event).unwrap();
        }
        let mut inbox = Inbox::new(inbox);
        let heard = hear(&mut inbox, Instant::now() + Duration::from_secs(10));
        let mut colours = Colours::default();
        colours.hear(Answer::Background(background));
        assert_eq!(heard, Answers {
            kitty: true,
            graphics: true,
            cell: Some(cell),
            window_pixels: None,
            colours,
        });
        assert!(is_typed(inbox.try_next().as_ref(), 'l'));
        assert!(is_typed(inbox.try_next().as_ref(), 's'));
        assert!(matches!(inbox.try_next(), Some(Event::Answered(Answer::Foreground(_)))));
        assert!(inbox.try_next().is_none());
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
