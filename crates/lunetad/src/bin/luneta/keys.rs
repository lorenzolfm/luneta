//! What luneta's terminal sends the program in it for a key, the mouse, a
//! paste or the focus, as an xterm would: luneta reads its own terminal's
//! input as events, and writes them out again for the modes the program asked
//! its terminal for. A program that asks for the kitty keyboard protocol has
//! the keys an xterm cannot tell apart, as `Shift Enter` from `Enter`, as the
//! protocol's first flag spells them, where luneta's own terminal tells them
//! apart for luneta.

use alacritty_terminal::term::TermMode;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// The numbers xterm sends `F5` to `F12` as, which skip 16 and 22.
const FUNCTION_NUMBERS: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];

/// The bytes for a key, or none for a key xterm sends nothing for, or its
/// release.
pub fn key(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)
        && let Some(sent) = kitty(key)
    {
        return Some(sent);
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let held = parameter(key.modifiers);
    let cursor = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
    let with_alt = |bytes: &[u8]| {
        let mut sent = Vec::with_capacity(bytes.len() + 1);
        if alt {
            sent.push(0x1b);
        }
        sent.extend_from_slice(bytes);
        sent
    };
    let sent = match key.code {
        KeyCode::Char(c) if control => with_alt(&[control_byte(c)?]),
        KeyCode::Char(c) => with_alt(c.encode_utf8(&mut [0; 4]).as_bytes()),
        KeyCode::Enter if mode.contains(TermMode::LINE_FEED_NEW_LINE) => with_alt(b"\r\n"),
        KeyCode::Enter => with_alt(b"\r"),
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => with_alt(b"\t"),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if control => with_alt(b"\x08"),
        KeyCode::Backspace => with_alt(b"\x7f"),
        KeyCode::Esc => with_alt(b"\x1b"),
        KeyCode::Null => vec![0],
        KeyCode::Up => lettered(cursor, 'A', held),
        KeyCode::Down => lettered(cursor, 'B', held),
        KeyCode::Right => lettered(cursor, 'C', held),
        KeyCode::Left => lettered(cursor, 'D', held),
        KeyCode::Home => lettered(cursor, 'H', held),
        KeyCode::End => lettered(cursor, 'F', held),
        KeyCode::Insert => numbered(2, held),
        KeyCode::Delete => numbered(3, held),
        KeyCode::PageUp => numbered(5, held),
        KeyCode::PageDown => numbered(6, held),
        // `F1` to `F4` go after `ESC O` whatever the cursor's mode.
        KeyCode::F(n @ 1..=4) => lettered("\x1bO", char::from(b'P' + n - 1), held),
        KeyCode::F(n @ 5..=12) => numbered(FUNCTION_NUMBERS[usize::from(n - 5)], held),
        _ => return None,
    };
    Some(sent)
}

/// xterm's parameter for the modifiers held with a key that has an escape
/// sequence of its own: 1, for none, and a bit for each.
fn parameter(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
}

/// A key xterm sends as a letter, `last`, after `introducer`: `ESC [`, or
/// `ESC O`. With a modifier held, the letter goes after `ESC [ 1 ;` and the
/// modifiers' parameter, which only that form has room for.
fn lettered(introducer: &str, last: char, held: u8) -> Vec<u8> {
    assert!((1..=8).contains(&held), "xterm's parameter is 1 and a bit for each modifier");
    if held > 1 {
        return format!("\x1b[1;{held}{last}").into_bytes();
    }
    format!("{introducer}{last}").into_bytes()
}

/// A key xterm sends as `number` and a `~` after `ESC [`, with the modifiers'
/// parameter between the two while one is held.
fn numbered(number: u8, held: u8) -> Vec<u8> {
    assert!((1..=8).contains(&held), "xterm's parameter is 1 and a bit for each modifier");
    if held > 1 {
        return format!("\x1b[{number};{held}~").into_bytes();
    }
    format!("\x1b[{number}~").into_bytes()
}

/// The bytes the kitty keyboard protocol has for a key that xterm's would not
/// tell apart, with its first flag, "disambiguate escape codes", the one
/// zellij asks for: `Esc`, a character typed with a modifier other than
/// `Shift`, and `Enter`, `Tab` and `Backspace` with any. None for every other
/// key, which goes as xterm sends it.
fn kitty(key: &KeyEvent) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    // xterm's parameter, and a bit for `Super`, which kitty's has room for.
    let held = parameter(modifiers) + 8 * u8::from(modifiers.contains(KeyModifiers::SUPER));
    let code = match key.code {
        KeyCode::Esc => 27,
        KeyCode::Enter if held > 1 => 13,
        KeyCode::Tab if held > 1 => 9,
        KeyCode::BackTab => 9,
        KeyCode::Backspace if held > 1 => 127,
        // A character with `Shift` alone is the text it types.
        KeyCode::Char(c) if held > 2 || (held == 2 && !modifiers.contains(KeyModifiers::SHIFT)) => {
            u32::from(c.to_ascii_lowercase())
        },
        _ => return None,
    };
    // `BackTab` is `Tab` with `Shift`, whether or not the terminal says so.
    let held = if key.code == KeyCode::BackTab { 1 + ((held - 1) | 1) } else { held };
    Some(
        if held > 1 { format!("\x1b[{code};{held}u") } else { format!("\x1b[{code}u") }
            .into_bytes(),
    )
}

/// The byte a terminal sends for a character typed with `Ctrl`, as xterm
/// maps them. None for a character it has none for.
fn control_byte(c: char) -> Option<u8> {
    let Ok(typed) = u8::try_from(c.to_ascii_lowercase()) else {
        return None;
    };
    let byte = match typed {
        b'a'..=b'z' => typed - b'a' + 1,
        b' ' | b'@' | b'2' => 0,
        b'[' | b'3' => 0x1b,
        b'\\' | b'4' => 0x1c,
        b']' | b'5' => 0x1d,
        b'^' | b'6' => 0x1e,
        b'_' | b'7' | b'/' => 0x1f,
        b'?' | b'8' => 0x7f,
        _ => return None,
    };
    // A C0 control, or `DEL`.
    assert!(byte < 0x20 || byte == 0x7f, "{byte:#04x}");
    Some(byte)
}

/// The bytes for the mouse at `column` and `row` of the program's screen,
/// from 0, or none when the program does not ask for this kind of event.
/// With no mouse asked for, the wheel scrolls a full-screen program by the
/// arrow keys, as the program asks with `ALTERNATE_SCROLL`.
pub fn mouse(event: MouseEvent, column: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    let wheel = match event.kind {
        MouseEventKind::ScrollUp => Some(('A', 64)),
        MouseEventKind::ScrollDown => Some(('B', 65)),
        MouseEventKind::ScrollLeft => Some(('D', 66)),
        MouseEventKind::ScrollRight => Some(('C', 67)),
        _ => None,
    };
    if !mode.intersects(TermMode::MOUSE_MODE) {
        let (arrow, _) = wheel?;
        if !mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            return None;
        }
        let prefix = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
        return Some(format!("{prefix}{arrow}").repeat(3).into_bytes());
    }
    let button = |button: MouseButton| match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (code, released) = match event.kind {
        MouseEventKind::Down(pressed) => (button(pressed), false),
        MouseEventKind::Up(pressed) => (button(pressed), true),
        MouseEventKind::Drag(pressed)
            if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) =>
        {
            (button(pressed) + 32, false)
        },
        MouseEventKind::Moved if mode.contains(TermMode::MOUSE_MOTION) => (3 + 32, false),
        MouseEventKind::Drag(_) | MouseEventKind::Moved => return None,
        _ => (wheel?.1, false),
    };
    let modifiers = event.modifiers;
    let code = code
        + 4 * u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 8 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 16 * u8::from(modifiers.contains(KeyModifiers::CONTROL));
    let (x, y) = (u32::from(column) + 1, u32::from(row) + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let last = if released { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{last}").into_bytes());
    }
    // The first encoding: a release has no button, and a place past 223 has no
    // byte to say it in.
    let code = if released { 3 | (code & !3) } else { code };
    let x = u8::try_from(x + 32).ok()?;
    let y = u8::try_from(y + 32).ok()?;
    Some(vec![0x1b, b'[', b'M', code + 32, x, y])
}

/// The bytes for text pasted in: bracketed, when the program asks for that,
/// with any escape taken out of it, so that the text cannot end the bracket
/// and be read as keys.
pub fn paste(text: &str, mode: TermMode) -> Vec<u8> {
    if !mode.contains(TermMode::BRACKETED_PASTE) {
        return text.as_bytes().to_vec();
    }
    let text = text.replace('\x1b', "");
    format!("\x1b[200~{text}\x1b[201~").into_bytes()
}

/// The bytes for the program's terminal taking the focus, or losing it, when
/// the program asks to be told.
pub fn focus(gained: bool, mode: TermMode) -> Option<&'static [u8]> {
    if !mode.contains(TermMode::FOCUS_IN_OUT) {
        return None;
    }
    Some(if gained { b"\x1b[I" } else { b"\x1b[O" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
        key(&KeyEvent::new(code, modifiers), TermMode::empty())
    }

    #[test]
    fn a_character_is_its_bytes_and_alt_puts_an_escape_before_it() {
        assert_eq!(pressed(KeyCode::Char('a'), KeyModifiers::NONE), Some(b"a".to_vec()));
        assert_eq!(pressed(KeyCode::Char('A'), KeyModifiers::SHIFT), Some(b"A".to_vec()));
        assert_eq!(pressed(KeyCode::Char('é'), KeyModifiers::NONE), Some("é".into()));
        assert_eq!(pressed(KeyCode::Char('x'), KeyModifiers::ALT), Some(b"\x1bx".to_vec()));
    }

    /// What crossterm reads a control byte as is written back as that byte.
    #[test]
    fn a_control_key_is_the_byte_it_came_as() {
        let control = KeyModifiers::CONTROL;
        assert_eq!(pressed(KeyCode::Char('b'), control), Some(vec![0x02]));
        assert_eq!(pressed(KeyCode::Char('h'), control), Some(vec![0x08]));
        assert_eq!(pressed(KeyCode::Char(' '), control), Some(vec![0x00]));
        assert_eq!(pressed(KeyCode::Char('4'), control), Some(vec![0x1c]));
        assert_eq!(pressed(KeyCode::Char('7'), control), Some(vec![0x1f]));
        let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(pressed(KeyCode::Char('c'), both), Some(vec![0x1b, 0x03]));
        assert_eq!(pressed(KeyCode::Char('é'), control), None);
    }

    #[test]
    fn an_arrow_follows_the_cursor_mode_unless_a_modifier_is_held() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key(&up, TermMode::empty()), Some(b"\x1b[A".to_vec()));
        assert_eq!(key(&up, TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        let control_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(key(&control_up, TermMode::APP_CURSOR), Some(b"\x1b[1;5A".to_vec()));
        let shift_alt = KeyModifiers::SHIFT | KeyModifiers::ALT;
        assert_eq!(pressed(KeyCode::Left, shift_alt), Some(b"\x1b[1;4D".to_vec()));
    }

    #[test]
    fn the_keys_with_a_number_send_it() {
        assert_eq!(pressed(KeyCode::Delete, KeyModifiers::NONE), Some(b"\x1b[3~".to_vec()));
        assert_eq!(pressed(KeyCode::PageUp, KeyModifiers::SHIFT), Some(b"\x1b[5;2~".to_vec()));
        assert_eq!(pressed(KeyCode::F(1), KeyModifiers::NONE), Some(b"\x1bOP".to_vec()));
        assert_eq!(pressed(KeyCode::F(4), KeyModifiers::CONTROL), Some(b"\x1b[1;5S".to_vec()));
        assert_eq!(pressed(KeyCode::F(5), KeyModifiers::NONE), Some(b"\x1b[15~".to_vec()));
        assert_eq!(pressed(KeyCode::F(12), KeyModifiers::NONE), Some(b"\x1b[24~".to_vec()));
        assert_eq!(pressed(KeyCode::F(13), KeyModifiers::NONE), None);
    }

    #[test]
    fn the_keys_of_their_own_byte_send_it() {
        assert_eq!(pressed(KeyCode::Enter, KeyModifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(pressed(KeyCode::Enter, KeyModifiers::ALT), Some(b"\x1b\r".to_vec()));
        assert_eq!(pressed(KeyCode::Tab, KeyModifiers::NONE), Some(b"\t".to_vec()));
        assert_eq!(pressed(KeyCode::BackTab, KeyModifiers::SHIFT), Some(b"\x1b[Z".to_vec()));
        assert_eq!(pressed(KeyCode::Backspace, KeyModifiers::NONE), Some(b"\x7f".to_vec()));
        assert_eq!(pressed(KeyCode::Esc, KeyModifiers::NONE), Some(b"\x1b".to_vec()));
    }

    /// The keys xterm cannot tell apart go as kitty spells them to a program
    /// that asked, and the rest as xterm sends them.
    #[test]
    fn a_program_that_asks_for_kitty_s_keys_has_them() {
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        let sent =
            |code: KeyCode, modifiers: KeyModifiers| key(&KeyEvent::new(code, modifiers), kitty);
        assert_eq!(sent(KeyCode::Enter, KeyModifiers::SHIFT), Some(b"\x1b[13;2u".to_vec()));
        assert_eq!(sent(KeyCode::Enter, KeyModifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(sent(KeyCode::Esc, KeyModifiers::NONE), Some(b"\x1b[27u".to_vec()));
        assert_eq!(sent(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(b"\x1b[99;5u".to_vec()));
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(sent(KeyCode::Char('A'), both), Some(b"\x1b[97;6u".to_vec()));
        assert_eq!(sent(KeyCode::Char('x'), KeyModifiers::ALT), Some(b"\x1b[120;3u".to_vec()));
        assert_eq!(sent(KeyCode::Char('A'), KeyModifiers::SHIFT), Some(b"A".to_vec()));
        assert_eq!(sent(KeyCode::BackTab, KeyModifiers::SHIFT), Some(b"\x1b[9;2u".to_vec()));
        assert_eq!(sent(KeyCode::Tab, KeyModifiers::NONE), Some(b"\t".to_vec()));
        assert_eq!(sent(KeyCode::Up, KeyModifiers::CONTROL), Some(b"\x1b[1;5A".to_vec()));
        let plain = key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), TermMode::empty());
        assert_eq!(plain, Some(b"\r".to_vec()));
    }

    #[test]
    fn a_release_sends_nothing() {
        let mut released = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        released.kind = KeyEventKind::Release;
        assert_eq!(key(&released, TermMode::empty()), None);
    }

    fn mouse_at(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent { kind, column: 0, row: 0, modifiers }
    }

    #[test]
    fn the_mouse_goes_as_the_program_asked_for_it() {
        let click = mouse_at(MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE);
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse(click, 4, 9, sgr), Some(b"\x1b[<0;5;10M".to_vec()));
        let release = mouse_at(MouseEventKind::Up(MouseButton::Right), KeyModifiers::CONTROL);
        assert_eq!(mouse(release, 0, 0, sgr), Some(b"\x1b[<18;1;1m".to_vec()));
        let old = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(mouse(click, 0, 0, old), Some(vec![0x1b, b'[', b'M', 32, 33, 33]));
        assert_eq!(mouse(release, 0, 0, old), Some(vec![0x1b, b'[', b'M', 32 + 19, 33, 33]));
        assert_eq!(mouse(click, 300, 0, old), None);
        assert_eq!(mouse(click, 0, 0, TermMode::empty()), None);
    }

    #[test]
    fn moving_goes_only_to_a_program_that_asked_for_it() {
        let drag = mouse_at(MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE);
        let moved = mouse_at(MouseEventKind::Moved, KeyModifiers::NONE);
        let clicks = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse(drag, 0, 0, clicks), None);
        assert_eq!(mouse(drag, 0, 0, clicks | TermMode::MOUSE_DRAG), Some(b"\x1b[<32;1;1M".into()));
        assert_eq!(mouse(moved, 0, 0, clicks | TermMode::MOUSE_DRAG), None);
        assert_eq!(
            mouse(moved, 0, 0, clicks | TermMode::MOUSE_MOTION),
            Some(b"\x1b[<35;1;1M".into())
        );
    }

    #[test]
    fn the_wheel_is_arrows_to_a_full_screen_program_with_no_mouse() {
        let up = mouse_at(MouseEventKind::ScrollUp, KeyModifiers::NONE);
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse(up, 0, 0, sgr), Some(b"\x1b[<64;1;1M".to_vec()));
        let scrolling = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(mouse(up, 0, 0, scrolling), Some(b"\x1b[A\x1b[A\x1b[A".to_vec()));
        assert_eq!(mouse(up, 0, 0, TermMode::ALT_SCREEN), None);
    }

    #[test]
    fn a_bracketed_paste_cannot_end_its_bracket() {
        assert_eq!(paste("ls\r", TermMode::empty()), b"ls\r".to_vec());
        let sneaky = "a\x1b[201~rm -rf ~\r";
        assert_eq!(
            paste(sneaky, TermMode::BRACKETED_PASTE),
            b"\x1b[200~a[201~rm -rf ~\r\x1b[201~".to_vec()
        );
    }

    #[test]
    fn the_focus_is_said_only_when_asked() {
        assert_eq!(focus(true, TermMode::empty()), None);
        assert_eq!(focus(true, TermMode::FOCUS_IN_OUT), Some(&b"\x1b[I"[..]));
        assert_eq!(focus(false, TermMode::FOCUS_IN_OUT), Some(&b"\x1b[O"[..]));
    }
}
