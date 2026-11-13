//! What a client does in a pane, as the bytes its program reads: keys, the
//! mouse, a paste and the focus, each in the way the program last asked for.

use libghostty_vt::key::{self, Mods};
use libghostty_vt::mouse::{self, EncoderSize, Position};
use libghostty_vt::mode::Mode;
use libghostty_vt::{Error, focus, paste};

use super::Screen;

/// How wide and high a cell is taken to be, in the pixels the mouse's encoder
/// counts in: any size, as a client says where it is in cells.
const CELL_PIXELS: u32 = 2;

/// What a mouse button did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Press(mouse::Button),
    Release(mouse::Button),
    /// A move, with the button held, if one is.
    Motion(Option<mouse::Button>),
}

/// A turn of the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    Up,
    Down,
    Left,
    Right,
}

pub(super) struct Encoders {
    keys: key::Encoder<'static>,
    key: key::Event<'static>,
    mice: mouse::Encoder<'static>,
    mouse: mouse::Event<'static>,
    held: Option<mouse::Button>,
}

impl Encoders {
    pub(super) fn new() -> Result<Encoders, Error> {
        let mut mice = mouse::Encoder::new()?;
        mice.set_track_last_cell(true);
        Ok(Encoders {
            keys: key::Encoder::new()?,
            key: key::Event::new()?,
            mice,
            mouse: mouse::Event::new()?,
            held: None,
        })
    }
}

impl Screen {
    /// What the program reads of a key: `key`, with `mods` held, that types
    /// `text`, and types `unshifted` with no modifier held.
    pub fn key(
        &mut self,
        action: key::Action,
        key: key::Key,
        mods: Mods,
        text: Option<&str>,
        unshifted: Option<char>,
    ) -> Result<Vec<u8>, Error> {
        let encoders = &mut self.input;
        let event = encoders.key.set_action(action).set_key(key).set_mods(mods);
        event.set_consumed_mods(Mods::empty()).set_utf8(text);
        event.set_unshifted_codepoint(unshifted.unwrap_or('\0'));
        let mut bytes = Vec::new();
        encoders.keys.set_options_from_terminal(self.answering.terminal());
        encoders.keys.encode_to_vec(&encoders.key, &mut bytes)?;
        Ok(bytes)
    }

    /// What the program reads of the mouse at `column` and `row`, where it
    /// takes the mouse: nothing where it does not.
    pub fn mouse(
        &mut self,
        click: Click,
        mods: Mods,
        column: u16,
        row: u16,
    ) -> Result<Vec<u8>, Error> {
        let (action, button) = match click {
            Click::Press(button) => (mouse::Action::Press, Some(button)),
            Click::Release(button) => (mouse::Action::Release, Some(button)),
            Click::Motion(held) => (mouse::Action::Motion, held),
        };
        self.input.held = match click {
            Click::Press(button) => Some(button),
            Click::Release(_) => None,
            Click::Motion(held) => held,
        };
        self.encoded(action, button, mods, column, row)
    }

    /// What the program reads of a turn of the wheel at `column` and `row`,
    /// where it takes the mouse.
    pub fn wheel(
        &mut self,
        wheel: Wheel,
        mods: Mods,
        column: u16,
        row: u16,
    ) -> Result<Vec<u8>, Error> {
        let button = match wheel {
            Wheel::Up => mouse::Button::Four,
            Wheel::Down => mouse::Button::Five,
            Wheel::Left => mouse::Button::Six,
            Wheel::Right => mouse::Button::Seven,
        };
        self.encoded(mouse::Action::Press, Some(button), mods, column, row)
    }

    /// Whether a turn of the wheel is for the program as the arrow keys: it
    /// asked for that (mode 1007), on the alternate screen, and does not take
    /// the mouse.
    #[must_use]
    pub fn wheel_is_arrows(&self) -> bool {
        let alternate = self.mode(Mode::ALT_SCREEN_SAVE) || self.mode(Mode::ALT_SCREEN);
        self.mode(Mode::ALT_SCROLL) && alternate && !self.tracks_mouse()
    }

    fn encoded(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        mods: Mods,
        column: u16,
        row: u16,
    ) -> Result<Vec<u8>, Error> {
        let terminal = self.answering.terminal();
        let size = EncoderSize {
            screen_width: u32::from(self.size.columns.get()) * CELL_PIXELS,
            screen_height: u32::from(self.size.rows.get()) * CELL_PIXELS,
            cell_width: CELL_PIXELS,
            cell_height: CELL_PIXELS,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        let middle = |cell: u16| (u32::from(cell) * CELL_PIXELS) as f32 + 0.5;
        let encoders = &mut self.input;
        encoders.mouse.set_action(action).set_button(button).set_mods(mods);
        encoders.mouse.set_position(Position { x: middle(column), y: middle(row) });
        encoders
            .mice
            .set_options_from_terminal(terminal)
            .set_size(size)?
            .set_any_button_pressed(encoders.held.is_some());
        let mut bytes = Vec::new();
        encoders.mice.encode_to_vec(&encoders.mouse, &mut bytes)?;
        Ok(bytes)
    }

    /// What the program reads of `text` pasted: bracketed, where it asked for
    /// that (mode 2004), and with the control characters that could end a
    /// bracket made spaces.
    pub fn paste(&self, text: &str) -> Result<Vec<u8>, Error> {
        let bracketed = self.answering.terminal().mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        let mut bytes = vec![0; data.len() + 16];
        match paste::encode(&mut data, bracketed, &mut bytes) {
            Ok(written) => bytes.truncate(written),
            Err(Error::OutOfSpace { required }) => {
                bytes.resize(required, 0);
                let mut data = text.as_bytes().to_vec();
                let written = paste::encode(&mut data, bracketed, &mut bytes)?;
                bytes.truncate(written);
            },
            Err(error) => return Err(error),
        }
        Ok(bytes)
    }

    /// What the program reads of the client gaining or losing the focus:
    /// nothing, unless it asked to be told (mode 1004).
    pub fn focus(&self, focused: bool) -> Result<Vec<u8>, Error> {
        if !self.answering.terminal().mode(Mode::FOCUS_EVENT)? {
            return Ok(Vec::new());
        }
        let event = if focused { focus::Event::Gained } else { focus::Event::Lost };
        let mut bytes = [0; 8];
        let written = event.encode(&mut bytes)?;
        Ok(bytes[..written].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use libghostty_vt::key::{Action, Key};

    use super::super::tests::{answered, size};
    use super::*;

    fn screen() -> Screen {
        Screen::new(size(10, 4), 0).unwrap()
    }

    fn typed(
        screen: &mut Screen,
        key: Key,
        mods: Mods,
        text: Option<&str>,
        unshifted: char,
    ) -> Vec<u8> {
        screen.key(Action::Press, key, mods, text, Some(unshifted)).unwrap()
    }

    /// A letter types itself, and Ctrl with it the control character, until
    /// the program asks for kitty's keys: then Ctrl a is spelled out.
    #[test]
    fn keys_follow_the_programs_keyboard_mode() {
        let mut screen = screen();
        assert_eq!(typed(&mut screen, Key::A, Mods::empty(), Some("a"), 'a'), b"a");
        assert_eq!(typed(&mut screen, Key::A, Mods::CTRL, None, 'a'), b"\x01");
        assert_eq!(answered(&mut screen, b"\x1b[>1u"), b"");
        assert_eq!(typed(&mut screen, Key::A, Mods::CTRL, None, 'a'), b"\x1b[97;5u");
    }

    /// The arrow keys follow the cursor keys mode (DECCKM).
    #[test]
    fn arrows_follow_the_cursor_keys_mode() {
        let mut screen = screen();
        let up = |screen: &mut Screen| {
            screen.key(Action::Press, Key::ArrowUp, Mods::empty(), None, None).unwrap()
        };
        assert_eq!(up(&mut screen), b"\x1b[A");
        assert_eq!(answered(&mut screen, b"\x1b[?1h"), b"");
        assert_eq!(up(&mut screen), b"\x1bOA");
    }

    /// A paste is bracketed only where the program asked.
    #[test]
    fn a_paste_is_bracketed_where_asked() {
        let mut screen = screen();
        assert_eq!(screen.paste("ls\n").unwrap(), b"ls\r");
        assert_eq!(answered(&mut screen, b"\x1b[?2004h"), b"");
        assert_eq!(screen.paste("ls\n").unwrap(), b"\x1b[200~ls\n\x1b[201~");
    }

    /// The mouse says nothing to a program that does not take it, and a click
    /// in SGR's words to one that asked.
    #[test]
    fn the_mouse_goes_to_a_program_that_takes_it() {
        let mut screen = screen();
        let click = Click::Press(mouse::Button::Left);
        assert_eq!(screen.mouse(click, Mods::empty(), 2, 1).unwrap(), b"");
        assert_eq!(answered(&mut screen, b"\x1b[?1000h\x1b[?1006h"), b"");
        assert_eq!(screen.mouse(click, Mods::empty(), 2, 1).unwrap(), b"\x1b[<0;3;2M");
    }

    /// The focus is told only to a program that asked.
    #[test]
    fn the_focus_is_told_where_asked() {
        let mut screen = screen();
        assert_eq!(screen.focus(true).unwrap(), b"");
        assert_eq!(answered(&mut screen, b"\x1b[?1004h"), b"");
        assert_eq!(screen.focus(false).unwrap(), b"\x1b[O");
    }
}
