use std::fmt;
use std::fs::File;
use std::io::{self, IsTerminal as _, Read as _};
use std::num::NonZeroU16;
use std::os::fd::AsFd as _;
use std::str;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
use signal_hook::flag;
use signal_hook::iterator::Signals;

use crate::answers::{self, Answer, CellPixels, Theme, WindowPixels};
use crate::host::Event;

const ESC: u8 = 0x1b;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

const COLOUR_LONGEST: usize = 32;

const APC_LONGEST: usize = 128;

const END_SIGNALS: [i32; 2] = [SIGHUP, SIGTERM];

pub fn read(events: Sender<Event>, ends: Ends) {
    resizes(events.clone());
    ends.tell(events.clone());
    thread::spawn(move || {
        if let Err(error) = listen(&events) {
            let _ = events.send(Event::InputEnded(error));
        }
    });
}

fn listen(events: &Sender<Event>) -> io::Result<()> {
    let mut terminal = terminal()?;
    let mut parser = Parser::default();
    let mut bytes = [0; 1024];
    loop {
        let read = match terminal.read(&mut bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let mut listening = true;
        parser.advance(&bytes[..read], read == bytes.len(), &mut |sent| {
            let event = match sent {
                Sent::Input(input) => Event::Input(input),
                Sent::Answer(answer) => Event::Answered(answer),
            };
            listening &= events.send(event).is_ok();
        });
        if !listening {
            return Ok(());
        }
    }
}

fn resizes(events: Sender<Event>) {
    let Ok(mut signals) = Signals::new([SIGWINCH]) else {
        return;
    };
    thread::spawn(move || {
        for _ in signals.forever() {
            let Ok((columns, rows)) = crossterm::terminal::size() else {
                continue;
            };
            if events.send(Event::Input(Input::Resize(columns, rows))).is_err() {
                return;
            }
        }
    });
}

pub struct Ends {
    signals: Signals,
}

impl Ends {
    pub fn heard() -> io::Result<Ends> {
        let signals = Signals::new(END_SIGNALS)?;
        let heard_once = Arc::new(AtomicBool::new(false));
        for signal in END_SIGNALS {
            flag::register_conditional_default(signal, Arc::clone(&heard_once))?;
            flag::register(signal, Arc::clone(&heard_once))?;
        }
        Ok(Ends { signals })
    }

    pub fn after<T>(&mut self, step: impl FnOnce() -> T) -> Result<T, Ended> {
        let done = step();
        self.signals.pending().next().map(Ended::by).map_or(Ok(done), Err)
    }

    fn tell(mut self, events: Sender<Event>) {
        thread::spawn(move || {
            if let Some(signal) = self.signals.forever().next() {
                let ended = Ended::by(signal);
                let _ = events.send(Event::InputEnded(io::Error::other(ended.to_string())));
            }
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Ended(i32);

impl Ended {
    fn by(signal: i32) -> Ended {
        assert!(END_SIGNALS.contains(&signal), "signal {signal} is not one lune ends by");
        Ended(signal)
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ended by signal {}", self.0)
    }
}

fn terminal() -> io::Result<File> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Ok(File::from(stdin.as_fd().try_clone_to_owned()?));
    }
    File::open("/dev/tty")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Sent {
    Input(Input),
    Answer(Answer),
}

enum Parsed {
    Partial,
    Unknown,
    Whole(Sent),
    Split(Sent, usize),
}

#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    read: bool,
}

impl Parser {
    fn advance(&mut self, bytes: &[u8], more: bool, sent: &mut impl FnMut(Sent)) {
        if std::mem::replace(&mut self.read, true) {
            self.take(bytes, more, sent);
            return;
        }
        let typed: Vec<u8> =
            bytes.iter().map(|&byte| if byte == b'\n' { b'\r' } else { byte }).collect();
        self.take(&typed, more, sent);
    }

    fn take(&mut self, bytes: &[u8], more: bool, sent: &mut impl FnMut(Sent)) {
        for (index, &byte) in bytes.iter().enumerate() {
            self.push(byte, more || index + 1 < bytes.len(), sent);
        }
    }

    fn push(&mut self, byte: u8, more: bool, sent: &mut impl FnMut(Sent)) {
        self.pending.push(byte);
        match parse(&self.pending, more) {
            Parsed::Partial => {},
            Parsed::Unknown => self.pending.clear(),
            Parsed::Whole(said) => {
                self.pending.clear();
                sent(said);
            },
            Parsed::Split(said, taken) => {
                let rest = self.pending.split_off(taken);
                self.pending.clear();
                sent(said);
                self.take(&rest, more, sent);
            },
        }
    }
}

fn parse(bytes: &[u8], more: bool) -> Parsed {
    match *bytes {
        [] => Parsed::Partial,
        [ESC] if more => Parsed::Partial,
        [ESC] | [ESC, ESC] => key(KeyCode::Esc, KeyModifiers::NONE),
        [ESC, b']' | b'_'] if !more => alt(parse(&bytes[1..], more)),
        [ESC, b'[', ..] => csi(bytes),
        [ESC, b'O', ..] => ss3(bytes),
        [ESC, b']', ..] => osc(bytes),
        [ESC, b'_', ..] => apc(bytes),
        [ESC, ..] => alt(parse(&bytes[1..], more)),
        [b'\r'] => key(KeyCode::Enter, KeyModifiers::NONE),
        [b'\t'] => key(KeyCode::Tab, KeyModifiers::NONE),
        [0x7f] => key(KeyCode::Backspace, KeyModifiers::NONE),
        [0] => key(KeyCode::Char(' '), KeyModifiers::CONTROL),
        [byte @ 0x01..=0x1a] => {
            key(KeyCode::Char(char::from(byte - 1 + b'a')), KeyModifiers::CONTROL)
        },
        [byte @ 0x1c..=0x1f] => {
            key(KeyCode::Char(char::from(byte - 0x1c + b'4')), KeyModifiers::CONTROL)
        },
        _ => character(bytes),
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Parsed {
    Parsed::Whole(Sent::Input(Input::Key(KeyEvent::new(code, modifiers))))
}

fn alt(parsed: Parsed) -> Parsed {
    match parsed {
        Parsed::Whole(Sent::Input(Input::Key(mut key))) => {
            key.modifiers |= KeyModifiers::ALT;
            Parsed::Whole(Sent::Input(Input::Key(key)))
        },
        parsed => parsed,
    }
}

fn character(bytes: &[u8]) -> Parsed {
    let length = match bytes[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Parsed::Unknown,
    };
    if bytes.len() < length {
        let going_on = bytes[1..].iter().all(|byte| byte & 0xc0 == 0x80);
        return if going_on { Parsed::Partial } else { Parsed::Unknown };
    }
    let Some(c) = str::from_utf8(bytes).ok().and_then(|text| text.chars().next()) else {
        return Parsed::Unknown;
    };
    let modifiers = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
    key(KeyCode::Char(c), modifiers)
}

fn ss3(bytes: &[u8]) -> Parsed {
    let [_, _, last] = *bytes else {
        return Parsed::Partial;
    };
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P'..=b'S' => KeyCode::F(1 + last - b'P'),
        _ => return Parsed::Unknown,
    };
    key(code, KeyModifiers::NONE)
}

fn csi(bytes: &[u8]) -> Parsed {
    if bytes.starts_with(PASTE_START) {
        return paste(bytes);
    }
    let Some(&first) = bytes.get(2) else {
        return Parsed::Partial;
    };
    let code = match first {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'S' => KeyCode::F(4),
        b'Z' => return key(KeyCode::BackTab, KeyModifiers::SHIFT),
        b'I' => return Parsed::Whole(Sent::Input(Input::FocusGained)),
        b'O' => return Parsed::Whole(Sent::Input(Input::FocusLost)),
        b'M' => return x10_mouse(bytes),
        b'[' => {
            return match bytes.get(3) {
                None => Parsed::Partial,
                Some(&last @ b'A'..=b'E') => key(KeyCode::F(1 + last - b'A'), KeyModifiers::NONE),
                Some(_) => Parsed::Unknown,
            };
        },
        b'0'..=b'9' | b';' | b'<' | b'?' => return sequence(bytes),
        _ => return Parsed::Unknown,
    };
    key(code, KeyModifiers::NONE)
}

fn sequence(bytes: &[u8]) -> Parsed {
    let last = bytes[bytes.len() - 1];
    if (0x20..=0x3f).contains(&last) {
        return Parsed::Partial;
    }
    if !(0x40..=0x7e).contains(&last) {
        return Parsed::Unknown;
    }
    let parameters = &bytes[2..bytes.len() - 1];
    match parameters {
        [b'<', parameters @ ..] => sgr_mouse(parameters, last),
        [b'?', parameters @ ..] => answer(parameters, last),
        _ => numbered(parameters, last),
    }
}

fn answer(parameters: &[u8], last: u8) -> Parsed {
    let answer = match (last, parameters) {
        (b'u', _) => Answer::Kitty,
        (b'c', _) => Answer::Device,
        (b'n', b"997;1") => Answer::Turned(Theme::Dark),
        (b'n', b"997;2") => Answer::Turned(Theme::Light),
        _ => return Parsed::Unknown,
    };
    Parsed::Whole(Sent::Answer(answer))
}

fn numbered(parameters: &[u8], last: u8) -> Parsed {
    let Ok(parameters) = str::from_utf8(parameters) else {
        return Parsed::Unknown;
    };
    match last {
        b'~' => special_key(parameters),
        b'u' => kitty_key(parameters),
        b'M' => rxvt_mouse(parameters),
        b't' => window_report(parameters),
        _ => modified_key(parameters, last),
    }
}

fn window_report(parameters: &str) -> Parsed {
    let mut parts = parameters.split(';');
    let (Some(kind), Some(Ok(height)), Some(Ok(width)), None) = (
        parts.next(),
        parts.next().map(str::parse::<NonZeroU16>),
        parts.next().map(str::parse::<NonZeroU16>),
        parts.next(),
    ) else {
        return Parsed::Unknown;
    };
    let answer = match kind {
        "6" => Answer::CellSize(CellPixels { width, height }),
        "4" => Answer::WindowPixels(WindowPixels { width, height }),
        _ => return Parsed::Unknown,
    };
    Parsed::Whole(Sent::Answer(answer))
}

fn held(field: &str) -> Option<(KeyModifiers, KeyEventState, KeyEventKind)> {
    let mut parts = field.split(':');
    let mask: u16 = parts.next()?.parse().ok()?;
    let kind = match parts.next().map(str::parse::<u8>) {
        Some(Ok(2)) => KeyEventKind::Repeat,
        Some(Ok(3)) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    let bits = mask.saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if bits & bit != 0 {
            modifiers |= modifier;
        }
    }
    let mut state = KeyEventState::NONE;
    if bits & 64 != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if bits & 128 != 0 {
        state |= KeyEventState::NUM_LOCK;
    }
    Some((modifiers, state, kind))
}

const UNHELD: (KeyModifiers, KeyEventState, KeyEventKind) =
    (KeyModifiers::NONE, KeyEventState::NONE, KeyEventKind::Press);

fn modified_key(parameters: &str, last: u8) -> Parsed {
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Parsed::Unknown,
    };
    let (modifiers, _, kind) = match parameters.split(';').nth(1).and_then(held) {
        Some(held) => held,
        None => match parameters.chars().last().and_then(|digit| digit.to_digit(10)) {
            Some(digit) => held(&digit.to_string()).unwrap_or(UNHELD),
            None => return Parsed::Unknown,
        },
    };
    Parsed::Whole(Sent::Input(Input::Key(KeyEvent::new_with_kind(code, modifiers, kind))))
}

fn special_key(parameters: &str) -> Parsed {
    let mut numbers = parameters.split(';');
    let Some(Ok(number)) = numbers.next().map(str::parse::<u8>) else {
        return Parsed::Unknown;
    };
    let code = match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F(number - 10),
        17..=21 => KeyCode::F(number - 11),
        23..=26 => KeyCode::F(number - 12),
        28..=29 => KeyCode::F(number - 15),
        31..=34 => KeyCode::F(number - 17),
        _ => return Parsed::Unknown,
    };
    let (modifiers, state, kind) = numbers.next().and_then(held).unwrap_or(UNHELD);
    Parsed::Whole(Sent::Input(Input::Key(KeyEvent::new_with_kind_and_state(
        code, modifiers, kind, state,
    ))))
}

fn kitty_key(parameters: &str) -> Parsed {
    let mut fields = parameters.split(';');
    let mut codes = fields.next().unwrap_or_default().split(':');
    let Some(Ok(number)) = codes.next().map(str::parse::<u32>) else {
        return Parsed::Unknown;
    };
    let (mut modifiers, state, kind) = fields.next().and_then(held).unwrap_or(UNHELD);
    let (mut code, keypad) = if let Some(code) = keypad(number) {
        (code, KeyEventState::KEYPAD)
    } else {
        let code = match char::from_u32(number) {
            Some('\x1b') => KeyCode::Esc,
            Some('\r') => KeyCode::Enter,
            Some('\t') if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            Some('\t') => KeyCode::Tab,
            Some('\x7f') => KeyCode::Backspace,
            Some('\u{e000}'..='\u{f8ff}') | None => return Parsed::Unknown,
            Some(c) => KeyCode::Char(c),
        };
        (code, KeyEventState::NONE)
    };
    if modifiers.contains(KeyModifiers::SHIFT)
        && let Some(shifted) = codes.next().and_then(|code| code.parse().ok()).and_then(char::from_u32)
    {
        code = KeyCode::Char(shifted);
        modifiers.remove(KeyModifiers::SHIFT);
    }
    Parsed::Whole(Sent::Input(Input::Key(KeyEvent::new_with_kind_and_state(
        code,
        modifiers,
        kind,
        keypad | state,
    ))))
}

fn keypad(number: u32) -> Option<KeyCode> {
    Some(match number {
        57399..=57408 => KeyCode::Char(char::from_digit(number - 57399, 10)?),
        57409 => KeyCode::Char('.'),
        57410 => KeyCode::Char('/'),
        57411 => KeyCode::Char('*'),
        57412 => KeyCode::Char('-'),
        57413 => KeyCode::Char('+'),
        57414 => KeyCode::Enter,
        57415 => KeyCode::Char('='),
        57416 => KeyCode::Char(','),
        57417 => KeyCode::Left,
        57418 => KeyCode::Right,
        57419 => KeyCode::Up,
        57420 => KeyCode::Down,
        57421 => KeyCode::PageUp,
        57422 => KeyCode::PageDown,
        57423 => KeyCode::Home,
        57424 => KeyCode::End,
        57425 => KeyCode::Insert,
        57426 => KeyCode::Delete,
        57427 => KeyCode::KeypadBegin,
        _ => return None,
    })
}

fn sgr_mouse(parameters: &[u8], last: u8) -> Parsed {
    let (b'M' | b'm', Ok(parameters)) = (last, str::from_utf8(parameters)) else {
        return Parsed::Unknown;
    };
    let mut numbers = parameters.split(';');
    let (Some(Ok(code)), Some(Ok(column)), Some(Ok(row))) = (
        numbers.next().map(str::parse::<u8>),
        numbers.next().map(str::parse::<u16>),
        numbers.next().map(str::parse::<u16>),
    ) else {
        return Parsed::Unknown;
    };
    mouse(code, column, row, last == b'm')
}

fn rxvt_mouse(parameters: &str) -> Parsed {
    let mut numbers = parameters.split(';').map(str::parse::<u16>);
    let (Some(Ok(code)), Some(Ok(column)), Some(Ok(row))) =
        (numbers.next(), numbers.next(), numbers.next())
    else {
        return Parsed::Unknown;
    };
    let Some(code) = code.checked_sub(32).and_then(|code| u8::try_from(code).ok()) else {
        return Parsed::Unknown;
    };
    mouse(code, column, row, false)
}

fn x10_mouse(bytes: &[u8]) -> Parsed {
    let [_, _, _, code, column, row] = *bytes else {
        return Parsed::Partial;
    };
    let (Some(code), Some(column), Some(row)) =
        (code.checked_sub(32), column.checked_sub(32), row.checked_sub(32))
    else {
        return Parsed::Unknown;
    };
    mouse(code, column.into(), row.into(), false)
}

fn mouse(code: u8, column: u16, row: u16, released: bool) -> Parsed {
    let button = (code & 0b0000_0011) | ((code & 0b1100_0000) >> 4);
    let dragging = code & 0b0010_0000 != 0;
    let kind = match (button, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3..=5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return Parsed::Unknown,
    };
    let kind = match kind {
        MouseEventKind::Down(button) if released => MouseEventKind::Up(button),
        kind => kind,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in
        [(4, KeyModifiers::SHIFT), (8, KeyModifiers::ALT), (16, KeyModifiers::CONTROL)]
    {
        if code & bit != 0 {
            modifiers |= modifier;
        }
    }
    let (Some(column), Some(row)) = (column.checked_sub(1), row.checked_sub(1)) else {
        return Parsed::Unknown;
    };
    Parsed::Whole(Sent::Input(Input::Mouse(MouseEvent { kind, column, row, modifiers })))
}

fn paste(bytes: &[u8]) -> Parsed {
    let Some(text) = bytes.strip_prefix(PASTE_START).and_then(|rest| rest.strip_suffix(PASTE_END))
    else {
        return Parsed::Partial;
    };
    Parsed::Whole(Sent::Input(Input::Paste(String::from_utf8_lossy(text).into_owned())))
}

fn osc(bytes: &[u8]) -> Parsed {
    let typed = || {
        let alt = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT);
        Parsed::Split(Sent::Input(Input::Key(alt)), 2)
    };
    let (which, spelled) = match bytes[2..] {
        [] | [b'1'] | [b'1', b'0' | b'1'] => return Parsed::Partial,
        [b'1', which @ (b'0' | b'1'), b';', ref spelled @ ..] => (which, spelled),
        _ => return typed(),
    };
    match *spelled {
        [ref colour @ .., 0x07] | [ref colour @ .., ESC, b'\\'] => {
            let Some(colour) = answers::rgb(colour) else {
                return Parsed::Unknown;
            };
            let answer = match which {
                b'0' => Answer::Foreground(colour),
                _ => Answer::Background(colour),
            };
            Parsed::Whole(Sent::Answer(answer))
        },
        [.., ESC] => Parsed::Partial,
        [.., ESC, _] => typed(),
        [.., last] if !last.is_ascii_graphic() || spelled.len() > COLOUR_LONGEST => typed(),
        _ => Parsed::Partial,
    }
}

fn apc(bytes: &[u8]) -> Parsed {
    assert!(bytes.len() >= 2, "apc starts with ESC _");
    assert_eq!(bytes[..2], [ESC, b'_'], "apc prefix matches ESC _");
    let typed = || {
        let alt = KeyEvent::new(KeyCode::Char('_'), KeyModifiers::ALT);
        Parsed::Split(Sent::Input(Input::Key(alt)), 2)
    };
    let body = match bytes[2..] {
        [] => return Parsed::Partial,
        [b'G', ref body @ ..] => body,
        _ => return typed(),
    };
    match *body {
        [ref payload @ .., ESC, b'\\'] => {
            let ok = payload == b"i=31;OK"
                || (payload.starts_with(b"i=31,") && payload.ends_with(b";OK"));
            if ok {
                Parsed::Whole(Sent::Answer(Answer::Graphics))
            } else {
                Parsed::Unknown
            }
        },
        [.., ESC] => Parsed::Partial,
        [.., ESC, _] => typed(),
        [.., last] if !last.is_ascii_graphic() || body.len() > APC_LONGEST => typed(),
        _ => Parsed::Partial,
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::vte::ansi::Rgb;

    use super::*;

    fn sent(reads: &[&[u8]]) -> Vec<Sent> {
        let mut parser = Parser::default();
        let mut all = Vec::new();
        for bytes in reads {
            parser.advance(bytes, false, &mut |sent| all.push(sent));
        }
        all
    }

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> Sent {
        Sent::Input(Input::Key(KeyEvent::new(code, modifiers)))
    }

    fn typed(c: char) -> Sent {
        pressed(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn a_signal_that_comes_during_a_step_before_lune_reads_its_terminal_is_its_end() {
        let mut ends = Ends::heard().expect("lune hears its end signals");
        assert_eq!(ends.after(|| "done"), Ok("done"));
        let raised = || signal_hook::low_level::raise(SIGTERM).expect("a signal is sent");
        let ended = ends.after(raised).expect_err("the signal ends lune");
        assert_eq!(ended, Ended(SIGTERM));
        assert_eq!(ended.to_string(), "ended by signal 15");
    }

    #[test]
    fn an_answer_is_heard_as_one_wherever_it_comes_among_the_keys() {
        let foreground = Rgb { r: 0xc0, g: 0xca, b: 0xf5 };
        let background = Rgb { r: 0x1e, g: 0x1e, b: 0x2e };
        let all = sent(&[
            b"l\x1b]10;rgb:c0c0/caca/f5f5\x1b\\s\x1b[?1u\x1b[?62;22c\r",
            b"\x1b]11;rgb:1e/1e/2e\x07x",
        ]);
        assert_eq!(all, [
            typed('l'),
            Sent::Answer(Answer::Foreground(foreground)),
            typed('s'),
            Sent::Answer(Answer::Kitty),
            Sent::Answer(Answer::Device),
            pressed(KeyCode::Enter, KeyModifiers::NONE),
            Sent::Answer(Answer::Background(background)),
            typed('x'),
        ]);
    }

    #[test]
    fn an_enter_typed_before_lune_took_the_terminal_raw_is_an_enter() {
        let enter = pressed(KeyCode::Enter, KeyModifiers::NONE);
        let control_j = pressed(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(sent(&[b"ls\n", b"\n"]), [typed('l'), typed('s'), enter, control_j]);
    }

    #[test]
    fn an_answer_split_between_reads_is_one() {
        let all = sent(&[b"\x1b]11;rgb:1e1e/", b"1e1e/2e2e\x1b", b"\\"]);
        let background = Rgb { r: 0x1e, g: 0x1e, b: 0x2e };
        assert_eq!(all, [Sent::Answer(Answer::Background(background))]);
    }

    #[test]
    fn an_esc_close_bracket_that_is_no_answer_is_alt_close_bracket_and_the_keys_after_it() {
        let alt = pressed(KeyCode::Char(']'), KeyModifiers::ALT);
        assert_eq!(sent(&[b"\x1b]"]), std::slice::from_ref(&alt));
        assert_eq!(sent(&[b"\x1b]x"]), [alt.clone(), typed('x')]);
        assert_eq!(sent(&[b"\x1b]11;ab\r"]), [
            alt.clone(),
            typed('1'),
            typed('1'),
            typed(';'),
            typed('a'),
            typed('b'),
            pressed(KeyCode::Enter, KeyModifiers::NONE),
        ]);
        assert_eq!(sent(&[b"\x1b]1", b"2"]), [alt, typed('1'), typed('2')]);
    }

    #[test]
    fn a_turn_to_dark_or_light_is_heard_and_other_reports_go_whole() {
        assert_eq!(sent(&[b"\x1b[?997;1n"]), [Sent::Answer(Answer::Turned(Theme::Dark))]);
        assert_eq!(sent(&[b"\x1b[?997;2n"]), [Sent::Answer(Answer::Turned(Theme::Light))]);
        assert_eq!(sent(&[b"\x1b[?2026;2$yx"]), [typed('x')]);
    }

    #[test]
    fn kitty_graphics_and_cell_size_replies_are_heard() {
        let all = sent(&[
            b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[4;1000;1600tx",
            b"\x1b_Gi=31;ENOTSUP\x1b\\y",
        ]);
        let nz = |value| NonZeroU16::new(value).unwrap();
        assert_eq!(all, [
            Sent::Answer(Answer::Graphics),
            Sent::Answer(Answer::CellSize(CellPixels { width: nz(10), height: nz(20) })),
            Sent::Answer(Answer::WindowPixels(WindowPixels { width: nz(1600), height: nz(1000) })),
            typed('x'),
            typed('y'),
        ]);
        assert_eq!(sent(&[b"\x1b[6;0;10tz"]), [typed('z')]);
        let alt_underscore = pressed(KeyCode::Char('_'), KeyModifiers::ALT);
        assert_eq!(sent(&[b"\x1b_"]), std::slice::from_ref(&alt_underscore));
        assert_eq!(sent(&[b"\x1b_x"]), [alt_underscore, typed('x')]);
    }

    #[test]
    fn keys_are_read_as_crossterm_reads_them() {
        let none = KeyModifiers::NONE;
        let control = KeyModifiers::CONTROL;
        let shift = KeyModifiers::SHIFT;
        let alt = KeyModifiers::ALT;
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            (b"a", KeyCode::Char('a'), none),
            (b"A", KeyCode::Char('A'), shift),
            ("é".as_bytes(), KeyCode::Char('é'), none),
            ("€".as_bytes(), KeyCode::Char('€'), none),
            ("🦀".as_bytes(), KeyCode::Char('🦀'), none),
            (b"\x01", KeyCode::Char('a'), control),
            (b"\x00", KeyCode::Char(' '), control),
            (b"\x1c", KeyCode::Char('4'), control),
            (b"\r", KeyCode::Enter, none),
            (b"\t", KeyCode::Tab, none),
            (b"\x7f", KeyCode::Backspace, none),
            (b"\x1b", KeyCode::Esc, none),
            (b"\x1bx", KeyCode::Char('x'), alt),
            (b"\x1b\r", KeyCode::Enter, alt),
            (b"\x1b[A", KeyCode::Up, none),
            (b"\x1bOA", KeyCode::Up, none),
            (b"\x1b[1;5A", KeyCode::Up, control),
            (b"\x1b[5A", KeyCode::Up, control),
            (b"\x1b[3~", KeyCode::Delete, none),
            (b"\x1b[5;2~", KeyCode::PageUp, shift),
            (b"\x1bOP", KeyCode::F(1), none),
            (b"\x1b[1;5R", KeyCode::F(3), control),
            (b"\x1b[24~", KeyCode::F(12), none),
            (b"\x1b[Z", KeyCode::BackTab, shift),
            (b"\x1b[27u", KeyCode::Esc, none),
            (b"\x1b[13;2u", KeyCode::Enter, shift),
            (b"\x1b[99;5u", KeyCode::Char('c'), control),
            (b"\x1b[97;6u", KeyCode::Char('a'), control | shift),
            (b"\x1b[9;2u", KeyCode::BackTab, shift),
            (b"\x1b[1;5P", KeyCode::F(1), control),
        ];
        for &(bytes, code, modifiers) in cases {
            assert_eq!(sent(&[bytes]), [pressed(code, modifiers)], "{bytes:?}");
        }
        let keypad = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('0'),
            none,
            KeyEventKind::Press,
            KeyEventState::KEYPAD,
        );
        assert_eq!(sent(&[b"\x1b[57399u"]), [Sent::Input(Input::Key(keypad))]);
        for bytes in [&b"\x1b[57363u"[..], b"\x1b[57376;5u", b"\x1b[57428u"] {
            assert_eq!(sent(&[bytes]), [], "{bytes:?}");
        }
        let mut parser = Parser::default();
        let mut all = Vec::new();
        parser.advance(b"\x1b", true, &mut |sent| all.push(sent));
        parser.advance(b"x", false, &mut |sent| all.push(sent));
        assert_eq!(all, [pressed(KeyCode::Char('x'), alt)]);
    }

    #[test]
    fn an_introducer_a_read_ends_on_waits_for_the_rest() {
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(sent(&[b"\x1b[", b"<35;3;4M"]), [Sent::Input(Input::Mouse(moved))]);
        let paste = Sent::Input(Input::Paste("ls\r".to_owned()));
        assert_eq!(sent(&[b"\x1b[", b"200~ls\r\x1b[201~"]), [paste]);
        assert_eq!(sent(&[b"\x1bO", b"A"]), [pressed(KeyCode::Up, KeyModifiers::NONE)]);
    }

    #[test]
    fn a_character_a_read_cuts_waits_for_the_rest() {
        let crab = "🦀".as_bytes();
        assert_eq!(sent(&[&crab[..2], &crab[2..]]), [typed('🦀')]);
    }

    #[test]
    fn the_mouse_the_focus_and_pastes_are_read() {
        let at = |kind, column, row, modifiers| {
            Sent::Input(Input::Mouse(MouseEvent { kind, column, row, modifiers }))
        };
        let left = MouseButton::Left;
        let cases: &[(&[u8], Sent)] = &[
            (b"\x1b[<0;5;10M", at(MouseEventKind::Down(left), 4, 9, KeyModifiers::NONE)),
            (b"\x1b[<0;5;10m", at(MouseEventKind::Up(left), 4, 9, KeyModifiers::NONE)),
            (b"\x1b[<18;1;1M", at(MouseEventKind::Down(MouseButton::Right), 0, 0, KeyModifiers::CONTROL)),
            (b"\x1b[<32;3;4M", at(MouseEventKind::Drag(left), 2, 3, KeyModifiers::NONE)),
            (b"\x1b[<35;3;4M", at(MouseEventKind::Moved, 2, 3, KeyModifiers::NONE)),
            (b"\x1b[<65;1;1M", at(MouseEventKind::ScrollDown, 0, 0, KeyModifiers::NONE)),
            (b"\x1b[M !!", at(MouseEventKind::Down(left), 0, 0, KeyModifiers::NONE)),
            (b"\x1b[I", Sent::Input(Input::FocusGained)),
            (b"\x1b[O", Sent::Input(Input::FocusLost)),
            (b"\x1b[200~ls\x1b[A\r\x1b[201~", Sent::Input(Input::Paste("ls\x1b[A\r".to_owned()))),
        ];
        for (bytes, expected) in cases {
            assert_eq!(sent(&[*bytes]), std::slice::from_ref(expected), "{bytes:?}");
        }
    }
}
