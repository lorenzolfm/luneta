//! What luneta's own terminal answers of itself as luneta starts: whether it
//! speaks the kitty keyboard protocol, and the colours it draws in where a
//! program names none, which luneta answers zellij with when zellij asks its
//! terminal for them. luneta reads the answers itself, before anything else
//! reads the terminal: crossterm, which reads it from then on, would take them
//! for keys.

use std::fs::File;
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::str;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use alacritty_terminal::vte::{Params, Parser, Perform};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

/// What luneta asks: the foreground and background colours (OSC 10 and 11),
/// the kitty keyboard protocol's flags, and last what the device is, which
/// every terminal answers. A terminal answers in the order it is asked, so
/// that once it says what it is, all it was going to answer has come.
const QUESTIONS: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b[c";

/// The longest luneta waits for the answers, for a terminal that answers
/// nothing, not even what it is.
const WAIT_MAX: Duration = Duration::from_secs(1);

/// What luneta's terminal said of itself.
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
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
}

impl Colours {
    /// The colour alacritty numbers `index`, where it is one of these.
    pub fn get(self, index: usize) -> Option<Rgb> {
        if index == NamedColor::Foreground as usize {
            self.foreground
        } else if index == NamedColor::Background as usize {
            self.background
        } else {
            None
        }
    }
}

/// Asks luneta's terminal, which luneta has set raw, and waits `WAIT_MAX` at
/// the most for the answers. What it did not answer by then, it does not
/// speak or did not say, as for a terminal that cannot be asked.
pub fn ask() -> Answers {
    let mut heard = Heard::default();
    // Whatever went wrong, what was heard until then stands.
    let _ = listen(&mut heard);
    heard.answers
}

/// What luneta heard its terminal answer so far.
#[derive(Default)]
struct Heard {
    answers: Answers,
    /// Whether it said what it is, the last answer.
    done: bool,
}

impl Perform for Heard {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let [which, colour] = params else {
            return;
        };
        match *which {
            b"10" => self.answers.colours.foreground = rgb(colour),
            b"11" => self.answers.colours.background = rgb(colour),
            _ => {},
        }
    }

    fn csi_dispatch(&mut self, _params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        match (intermediates, action) {
            (b"?", 'u') => self.answers.kitty = true,
            (b"?", 'c') => self.done = true,
            _ => {},
        }
    }
}

/// Asks, and hears the answers until the last comes or `WAIT_MAX` is up.
fn listen(heard: &mut Heard) -> io::Result<()> {
    let mut terminal = input()?;
    // Waited on as crossterm waits on it for keys: with kqueue on macOS, whose
    // poll(2) says it does not support devices.
    let mut waiting = Poll::new()?;
    let fd = terminal.as_raw_fd();
    waiting.registry().register(&mut SourceFd(&fd), Token(0), Interest::READABLE)?;
    let mut ready = Events::with_capacity(1);
    let mut output = io::stdout().lock();
    output.write_all(QUESTIONS)?;
    output.flush()?;
    let deadline = Instant::now() + WAIT_MAX;
    let mut parser: Parser = Parser::new();
    let mut bytes = [0; 1024];
    while !heard.done {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        match waiting.poll(&mut ready, Some(left)) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        if ready.is_empty() {
            continue;
        }
        // mio tells of what the terminal sent once, and not again until it
        // sends more, so all of it is read at once. Keys typed as the answers
        // came go with them.
        match terminal.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(read) => parser.advance(heard, &bytes[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// luneta's terminal, to read the answers from where crossterm reads keys: its
/// standard input, where that is the terminal, and `/dev/tty` where not.
fn input() -> io::Result<File> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Ok(File::from(stdin.as_fd().try_clone_to_owned()?));
    }
    File::open("/dev/tty")
}

/// The colour X11 spells `rgb:R/G/B`, as terminals answer with: each of red,
/// green and blue in one to four hex digits, of which `f`, `ff`, `fff` and
/// `ffff` are all the most there is.
fn rgb(spelled: &[u8]) -> Option<Rgb> {
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
    use super::*;

    /// What luneta hears of `bytes`, read a byte at a time as from the
    /// terminal.
    fn heard(bytes: &[u8]) -> Heard {
        let mut heard = Heard::default();
        let mut parser: Parser = Parser::new();
        for byte in bytes {
            parser.advance(&mut heard, std::slice::from_ref(byte));
        }
        heard
    }

    #[test]
    fn a_terminal_that_answers_all_says_its_colours_and_kitty() {
        // A key typed before the answers came, the colours in four digits and
        // two, ended in `ESC \` and in BEL, kitty's flags, and what it is.
        let said = b"x\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b]11;rgb:1e/1e/2e\x07\x1b[?1u\x1b[?62;22c";
        let all = heard(said);
        let colours = Colours {
            foreground: Some(Rgb { r: 0xff, g: 0xff, b: 0xff }),
            background: Some(Rgb { r: 0x1e, g: 0x1e, b: 0x2e }),
        };
        assert_eq!(all.answers, Answers { kitty: true, colours });
        assert!(all.done);
        // Not done before the last byte of what it is.
        assert!(!heard(&said[..said.len() - 1]).done);
    }

    #[test]
    fn a_terminal_that_only_says_what_it_is_is_done_with_nothing_more() {
        let heard = heard(b"\x1b[?6c");
        assert_eq!(heard.answers, Answers::default());
        assert!(heard.done);
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
