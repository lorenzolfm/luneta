//! luneta's client of an own session, beside the sidebar in zellij's place:
//! what it does goes to luneta-mux as events, which the server encodes for
//! the pane's program as it last asked, and luneta draws the frames the
//! server sends of the pane the client is on. The server says where the
//! client is, as it comes and each time it moves.
//!
//! The client keeps the last frame's glyphs, each as wide as the server says,
//! and a drag over them selects their text, as a terminal's does: where the
//! pane's program takes the mouse, a drag selects only with `Shift` held.

use std::num::NonZeroU16;
use std::sync::mpsc::Sender;

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind,
};
use lunetad::mux::own::api::{self, to_server};
use lunetad::mux::own::attach::{
    self, Attach, Cursor, Frame, Glyph, Heard, Hello, Line, Rows, Shows, Size, To, Went,
};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::host::Event;

/// What the client last had of the pane it is on: its frame's rows, and
/// where its cursor is, and whether its program takes the mouse.
struct Screen {
    pane: u32,
    size: Size,
    lines: Vec<Line>,
    cursor: Option<Cursor>,
    tracks_mouse: bool,
}

/// A cell of the pane, by its row and its column, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spot {
    pub row: u16,
    pub column: u16,
}

/// What a drag over the pane selects: from the cell it went down on to the
/// one it is over, which it shows lit, and copies as it lets go, once it
/// has moved off the first. A selection of a pane the client left goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    pane: u32,
    from: Spot,
    to: Spot,
}

impl Selection {
    /// Its first cell and its last, in the order the text reads.
    fn ends(self) -> (Spot, Spot) {
        if self.to < self.from {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        }
    }

    /// Whether it holds `spot`.
    fn holds(self, spot: Spot) -> bool {
        let (first, last) = self.ends();
        first <= spot && spot <= last
    }
}

/// What luneta's client has of the pane it is on, as the server last said:
/// where it is, the pane's last frame, and what is selected of it.
#[derive(Default)]
struct View {
    went: Option<Went>,
    screen: Option<Screen>,
    selection: Option<Selection>,
    /// Whether the left button is down on the selection, which follows it.
    selecting: bool,
}

impl View {
    /// Takes in what the server said, but for what ends the client, which
    /// luneta takes in itself; or says why it does not follow, as `framed`
    /// says.
    fn heard(&mut self, heard: &Heard) -> Result<(), String> {
        match heard {
            Heard::Went(went) => {
                if self.went.is_none_or(|before| before.pane != went.pane) {
                    self.screen = None;
                    self.unselect();
                }
                self.went = Some(*went);
            },
            Heard::Frame(frame) => self.framed(frame)?,
            Heard::Attached | Heard::Stayed(_) | Heard::Ended | Heard::Lost(_) => {},
        }
        Ok(())
    }

    /// Takes in `frame`, where it is of the pane the client is on: the rows
    /// that changed of a frame of the size the last was, or all of them. One
    /// of another pane, which the client left, is dropped. One that changes
    /// rows of none it had, or of a frame of another size, does not follow:
    /// the server sends all the rows of a pane to a client new to it, or of
    /// a size new to it, and the rows that changed of the last frame sent.
    /// What was selected of a frame of another size goes.
    fn framed(&mut self, frame: &Frame) -> Result<(), String> {
        if self.went.is_none_or(|went| went.pane != frame.pane) {
            return Ok(());
        }
        let before = self.screen.take();
        let size = before.as_ref().map(|screen| screen.size);
        let lines = match (&frame.rows, before) {
            (Rows::Whole(lines), _) => lines.clone(),
            (Rows::Changed(rows), Some(screen)) if screen.size == frame.size => {
                let mut lines = screen.lines;
                for (index, line) in rows {
                    lines[usize::from(*index)] = line.clone();
                }
                lines
            },
            (Rows::Changed(_), before) => {
                let sides = |size: Size| format!("{}x{}", size.columns, size.rows);
                let had = before.as_ref().map_or_else(|| "none".to_owned(), |it| sides(it.size));
                self.screen = before;
                let sides = sides(frame.size);
                return Err(format!("a frame of {sides} changes the rows of {had}"));
            },
        };
        assert_eq!(lines.len(), usize::from(frame.size.rows.get()));
        if size != Some(frame.size) {
            self.unselect();
        }
        self.screen = Some(Screen {
            pane: frame.pane,
            size: frame.size,
            lines,
            cursor: frame.cursor,
            tracks_mouse: frame.tracks_mouse,
        });
        Ok(())
    }

    fn tracks_mouse(&self) -> bool {
        self.screen.as_ref().is_some_and(|screen| screen.tracks_mouse)
    }

    fn select_from(&mut self, at: Spot) {
        self.unselect();
        let Some(screen) = &self.screen else {
            return;
        };
        let at = within(at, screen.size);
        self.selection = Some(Selection { pane: screen.pane, from: at, to: at });
        self.selecting = true;
    }

    fn select_to(&mut self, at: Spot) {
        let (Some(screen), Some(selection), true) =
            (&self.screen, &mut self.selection, self.selecting)
        else {
            return;
        };
        selection.to = within(at, screen.size);
    }

    fn selected(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.selecting) {
            return None;
        }
        let selection = self.selection.filter(|selection| selection.from != selection.to);
        let text = selection.and_then(|selection| Some(text(self.screen.as_ref()?, selection)));
        let text = text.filter(|text| !text.is_empty());
        if text.is_none() {
            self.selection = None;
        }
        text
    }

    fn unselect(&mut self) {
        self.selection = None;
        self.selecting = false;
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let screen = self.screen.as_ref()?;
        let selection = self.selection.filter(|selection| selection.pane == screen.pane);
        for (row, line) in (0..area.height).zip(&screen.lines) {
            let mut column = 0;
            for glyph in line.glyphs() {
                if column >= area.width {
                    break;
                }
                let at = Spot { row, column };
                let lit = selection.is_some_and(|selection| selection.holds(at));
                paint(buffer, area, at, glyph, lit);
                column += glyph.shows.columns();
            }
        }
        let cursor = screen.cursor?;
        (cursor.row < area.height && cursor.column < area.width)
            .then(|| Position::new(area.x + cursor.column, area.y + cursor.row))
    }

    fn cursor_style(&self) -> (CursorShape, bool) {
        let cursor = self.screen.as_ref().and_then(|screen| screen.cursor);
        let Some(cursor) = cursor else {
            return (CursorShape::HollowBlock, false);
        };
        let shape = match cursor.shape {
            attach::CursorShape::Block => CursorShape::Block,
            attach::CursorShape::Bar => CursorShape::Beam,
            attach::CursorShape::Underline => CursorShape::Underline,
            attach::CursorShape::BlockHollow => CursorShape::HollowBlock,
        };
        (shape, cursor.blinking)
    }
}

/// luneta's client of an own session.
pub struct Attached {
    attach: Attach,
    /// Its number, of the programs and clients luneta started, which what the
    /// server says to it carries.
    number: u64,
    /// The size it said it is last.
    size: Size,
    view: View,
}

impl Attached {
    /// A client, numbered `number`, that says `hello`, and hands what the
    /// server says to luneta's loop, as `Event::Own`.
    pub fn start(number: u64, hello: Hello, events: &Sender<Event>) -> Attached {
        let events = events.clone();
        let attach = attach::attach(hello, move |heard| {
            // luneta is on its way out.
            let _ = events.send(Event::Own(number, heard));
        });
        Attached { attach, number, size: hello.size, view: View::default() }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    /// Takes in what the server said, but for what ends the client, which
    /// luneta takes in itself. Where it does not follow what the client had,
    /// luneta gives up on the client, which hears last that it is lost.
    pub fn heard(&mut self, heard: &Heard) {
        if let Err(why) = self.view.heard(heard) {
            self.attach.give_up(format!("luneta-mux said what luneta cannot follow: {why}"));
        }
    }

    /// Says the client is `columns` by `rows` now, where it was not: the pane
    /// takes the size of the client that last did anything in it.
    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) {
        let size = Size::within(columns, rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.attach.send(to_server::Event::Resize(size.said()));
    }

    /// Sends the pane `key`, as you typed it: none of a key the server has no
    /// name for. What was selected goes.
    pub fn key(&mut self, key: &KeyEvent) {
        self.view.unselect();
        if let Some(key) = key_said(key) {
            self.attach.send(to_server::Event::Key(key));
        }
    }

    /// Pastes `text` in the pane. What was selected goes.
    pub fn paste(&mut self, text: &str) {
        self.view.unselect();
        self.attach.send(to_server::Event::Paste(api::Paste { text: text.to_owned() }));
    }

    /// Tells the pane its window took the focus, or lost it.
    pub fn focus(&self, focused: bool) {
        self.attach.send(to_server::Event::Focus(api::Focus { focused }));
    }

    /// Takes the client to `to`.
    pub fn go(&self, to: To) {
        self.attach.send(to_server::Event::Go(to.said()));
    }

    /// Sends the pane what the mouse did at `at`, of its cells. The wheel
    /// goes to the server, which scrolls the client back where the program
    /// does not take the mouse.
    pub fn mouse(&self, mouse: MouseEvent, at: Spot) {
        if let Some(mouse) = mouse_said(mouse, at) {
            self.attach.send(to_server::Event::Mouse(mouse));
        }
    }

    /// Whether a press of `mouse` selects, rather than goes to the pane: the
    /// left button, on a pane whose program does not take the mouse, or with
    /// `Shift` held.
    pub fn selects(&self, mouse: MouseEvent) -> bool {
        selects(self.view.tracks_mouse(), mouse)
    }

    /// The left button went down at `at`, to select: what was selected goes.
    pub fn select_from(&mut self, at: Spot) {
        self.view.select_from(at);
    }

    /// The button is dragged to `at`, as it selects.
    pub fn select_to(&mut self, at: Spot) {
        self.view.select_to(at);
    }

    /// The button is let go of: the text it selected, which stays lit, where
    /// it moved off the cell it went down on; or none, and nothing is.
    pub fn selected(&mut self) -> Option<String> {
        self.view.selected()
    }

    /// Lets go of what is selected, and of a selection under way.
    pub fn unselect(&mut self) {
        self.view.unselect();
    }

    /// Draws the pane's last frame into `area` of `buffer`, as much of it as
    /// `area` holds, with what is selected lit, and says where the cursor is
    /// on it, if it shows. What `area` holds past the frame shows nothing.
    pub fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        self.view.draw(buffer, area)
    }

    /// The cursor as the pane's program last styled it: its shape, and
    /// whether it blinks, as alacritty names them. A hollow block, which
    /// luneta's terminal draws as its own, before the first frame.
    pub fn cursor_style(&self) -> (CursorShape, bool) {
        self.view.cursor_style()
    }
}

/// Whether a press of `mouse` selects, on a pane whose program `tracks` the
/// mouse or not, as `Attached::selects` says.
fn selects(tracks: bool, mouse: MouseEvent) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && (!tracks || mouse.modifiers.contains(KeyModifiers::SHIFT))
}

/// `at`, made a cell of a frame of `size` where it is past its last row or
/// column.
fn within(at: Spot, size: Size) -> Spot {
    Spot { row: at.row.min(size.rows.get() - 1), column: at.column.min(size.columns.get() - 1) }
}

/// Draws `glyph` at `at` of a frame drawn into `area` of `buffer`, lit where
/// it is selected: as wide as the server says, and as nothing where it is
/// wide and `area` cuts it.
fn paint(buffer: &mut Buffer, area: Rect, at: Spot, glyph: &Glyph, lit: bool) {
    let columns = glyph.shows.columns();
    let fits = at.column + columns <= area.width;
    let (x, y) = (area.x + at.column, area.y + at.row);
    let Some(cell) = buffer.cell_mut((x, y)) else {
        return;
    };
    let mut style = glyph.style.as_ref().map_or_else(Style::new, styled);
    if lit {
        style = style.add_modifier(Modifier::REVERSED);
    }
    cell.set_style(style);
    match &glyph.shows {
        Shows::Narrow(grapheme) => {
            cell.set_symbol(grapheme);
            cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        },
        Shows::Wide(grapheme) if fits => {
            cell.set_symbol(grapheme);
            let two = NonZeroU16::new(2).expect("2 is not 0");
            cell.set_diff_option(CellDiffOption::ForcedWidth(two));
            if let Some(spacer) = buffer.cell_mut((x + 1, y)) {
                spacer.reset();
                spacer.set_style(style);
                spacer.set_diff_option(CellDiffOption::Skip);
            }
        },
        Shows::Blank | Shows::Wide(_) => {
            cell.set_symbol(" ");
        },
    }
}

/// The text `selection` holds of `screen`: each row's, from the first cell to
/// the last, with no blanks at the end of a row, and a line break between
/// rows. A wide glyph is in it where either of its columns is.
fn text(screen: &Screen, selection: Selection) -> String {
    let (first, last) = selection.ends();
    let mut rows = Vec::new();
    for row in first.row..=last.row {
        let Some(line) = screen.lines.get(usize::from(row)) else {
            break;
        };
        let from = if row == first.row { first.column } else { 0 };
        let to = if row == last.row { last.column } else { u16::MAX };
        let mut said = String::new();
        let mut column: u16 = 0;
        for glyph in line.glyphs() {
            let end = column + glyph.shows.columns() - 1;
            if end >= from && column <= to {
                match &glyph.shows {
                    Shows::Blank => said.push(' '),
                    Shows::Narrow(grapheme) | Shows::Wide(grapheme) => said.push_str(grapheme),
                }
            }
            column = end + 1;
        }
        rows.push(said.trim_end().to_owned());
    }
    rows.join("\n")
}

/// The style a glyph is drawn in: the colours a terminal names are left for
/// luneta's terminal to choose, as its theme does, as `hosted` leaves them.
/// Each colour is one a terminal has, as `attach` read it at the edge.
fn styled(style: &api::Style) -> Style {
    let colour = |colour: &Option<api::Colour>| {
        let byte = |value: u32| u8::try_from(value).expect("a colour's bytes read at the edge");
        match colour.as_ref().and_then(|it| it.is.as_ref()) {
            Some(api::colour::Is::Palette(index)) => Color::Indexed(byte(*index)),
            Some(api::colour::Is::Rgb(rgb)) => {
                Color::Rgb(byte(rgb.red), byte(rgb.green), byte(rgb.blue))
            },
            None => Color::Reset,
        }
    };
    let mut drawn = Style::new().fg(colour(&style.foreground)).bg(colour(&style.background));
    if style.underline_colour.is_some() {
        drawn = drawn.underline_color(colour(&style.underline_colour));
    }
    let underlined = !matches!(
        api::Underline::try_from(style.underline),
        Ok(api::Underline::None | api::Underline::Unspecified) | Err(_)
    );
    let modifiers = [
        (style.bold, Modifier::BOLD),
        (style.faint, Modifier::DIM),
        (style.italic, Modifier::ITALIC),
        (underlined, Modifier::UNDERLINED),
        (style.blink, Modifier::SLOW_BLINK),
        (style.inverse, Modifier::REVERSED),
        (style.invisible, Modifier::HIDDEN),
        (style.strikethrough, Modifier::CROSSED_OUT),
    ];
    for (set, modifier) in modifiers {
        if set {
            drawn = drawn.add_modifier(modifier);
        }
    }
    drawn
}

/// The modifiers held, as the server takes them.
fn modifiers_said(modifiers: KeyModifiers) -> api::Modifiers {
    api::Modifiers {
        shift: modifiers.contains(KeyModifiers::SHIFT),
        alt: modifiers.contains(KeyModifiers::ALT),
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        super_key: modifiers.contains(KeyModifiers::SUPER),
    }
}

/// `key`, as the server takes it: the key that types a character by the one
/// it types with no modifier, as far as luneta can tell, and what it types
/// with `Shift` alone; or a key the server names. None for one it does not,
/// as a media key's.
fn key_said(key: &KeyEvent) -> Option<api::Key> {
    use api::Named;
    let press = match key.kind {
        KeyEventKind::Press => api::Press::Down,
        KeyEventKind::Repeat => api::Press::Repeat,
        KeyEventKind::Release => api::Press::Up,
    };
    let mut modifiers = modifiers_said(key.modifiers);
    let named = |named: Named| Some((api::key::Key::Named(named.into()), String::new()));
    let (said, text) = match key.code {
        KeyCode::Char(typed) if typed.is_control() => return None,
        KeyCode::Char(typed) => {
            let mut lower = typed.to_lowercase();
            let unshifted = match (lower.next(), lower.next()) {
                (Some(lower), None) => lower,
                _ => typed,
            };
            (api::key::Key::Character(unshifted.into()), typed.to_string())
        },
        KeyCode::Enter => named(Named::Enter)?,
        KeyCode::Tab => named(Named::Tab)?,
        KeyCode::BackTab => {
            modifiers.shift = true;
            named(Named::Tab)?
        },
        KeyCode::Backspace => named(Named::Backspace)?,
        KeyCode::Esc => named(Named::Escape)?,
        KeyCode::Insert => named(Named::Insert)?,
        KeyCode::Delete => named(Named::Delete)?,
        KeyCode::Home => named(Named::Home)?,
        KeyCode::End => named(Named::End)?,
        KeyCode::PageUp => named(Named::PageUp)?,
        KeyCode::PageDown => named(Named::PageDown)?,
        KeyCode::Up => named(Named::Up)?,
        KeyCode::Down => named(Named::Down)?,
        KeyCode::Left => named(Named::Left)?,
        KeyCode::Right => named(Named::Right)?,
        KeyCode::F(number) => named(function(number)?)?,
        KeyCode::CapsLock => named(Named::CapsLock)?,
        KeyCode::NumLock => named(Named::NumLock)?,
        KeyCode::ScrollLock => named(Named::ScrollLock)?,
        KeyCode::PrintScreen => named(Named::PrintScreen)?,
        KeyCode::Pause => named(Named::Pause)?,
        KeyCode::Menu => named(Named::Menu)?,
        KeyCode::Modifier(modifier) => named(modifier_named(modifier)?)?,
        KeyCode::Null | KeyCode::KeypadBegin | KeyCode::Media(_) => return None,
    };
    Some(api::Key { press: press.into(), modifiers: Some(modifiers), key: Some(said), text })
}

/// The function key numbered `number`, of the twelve the server names.
fn function(number: u8) -> Option<api::Named> {
    use api::Named;
    const KEYS: [Named; 12] = [
        Named::F1,
        Named::F2,
        Named::F3,
        Named::F4,
        Named::F5,
        Named::F6,
        Named::F7,
        Named::F8,
        Named::F9,
        Named::F10,
        Named::F11,
        Named::F12,
    ];
    KEYS.get(usize::from(number).checked_sub(1)?).copied()
}

/// A modifier key, as the server names it: none for one it has no name for.
fn modifier_named(modifier: ModifierKeyCode) -> Option<api::Named> {
    use api::Named;
    Some(match modifier {
        ModifierKeyCode::LeftShift => Named::ShiftLeft,
        ModifierKeyCode::RightShift => Named::ShiftRight,
        ModifierKeyCode::LeftControl => Named::CtrlLeft,
        ModifierKeyCode::RightControl => Named::CtrlRight,
        ModifierKeyCode::LeftAlt => Named::AltLeft,
        ModifierKeyCode::RightAlt => Named::AltRight,
        ModifierKeyCode::LeftSuper => Named::SuperLeft,
        ModifierKeyCode::RightSuper => Named::SuperRight,
        _ => return None,
    })
}

/// What the mouse did at `at`, as the server takes it.
fn mouse_said(mouse: MouseEvent, at: Spot) -> Option<api::Mouse> {
    use api::mouse::Does;
    let button = |button: MouseButton| -> i32 {
        match button {
            MouseButton::Left => api::Button::Left,
            MouseButton::Middle => api::Button::Middle,
            MouseButton::Right => api::Button::Right,
        }
        .into()
    };
    let wheel = |wheel: api::Wheel| Some(Does::Wheel(wheel.into()));
    let does = match mouse.kind {
        MouseEventKind::Down(pressed) => Some(Does::Press(button(pressed))),
        MouseEventKind::Up(released) => Some(Does::Release(button(released))),
        MouseEventKind::Drag(held) => Some(Does::Motion(api::Motion { held: Some(button(held)) })),
        MouseEventKind::Moved => Some(Does::Motion(api::Motion { held: None })),
        MouseEventKind::ScrollUp => wheel(api::Wheel::Up),
        MouseEventKind::ScrollDown => wheel(api::Wheel::Down),
        MouseEventKind::ScrollLeft => wheel(api::Wheel::Left),
        MouseEventKind::ScrollRight => wheel(api::Wheel::Right),
    }?;
    Some(api::Mouse {
        modifiers: Some(modifiers_said(mouse.modifiers)),
        column: at.column.into(),
        row: at.row.into(),
        does: Some(does),
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use lunetad::mux::own::{OwnId, OwnKey};

    use super::*;

    fn glyph(shows: Shows) -> Glyph {
        Glyph { shows, style: None }
    }

    fn narrow(grapheme: &str) -> Glyph {
        glyph(Shows::Narrow(grapheme.to_owned()))
    }

    fn side(count: u16) -> NonZeroU16 {
        NonZeroU16::new(count).unwrap()
    }

    /// What the client has of pane 1, which shows `rows`, each of its glyphs.
    fn view(rows: Vec<Vec<Glyph>>) -> View {
        let columns = rows[0].iter().map(|glyph| glyph.shows.columns()).sum();
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|glyphs| Line::new(glyphs, false, side(columns)).unwrap())
            .collect();
        let size = Size { columns: side(columns), rows: side(u16::try_from(lines.len()).unwrap()) };
        let screen = Screen { pane: 1, size, lines, cursor: None, tracks_mouse: false };
        let session = OwnKey { id: OwnId::new(1).unwrap(), made: SystemTime::UNIX_EPOCH };
        let went = Went { session, tab: 1, pane: 1 };
        View { went: Some(went), screen: Some(screen), ..View::default() }
    }

    fn spot(row: u16, column: u16) -> Spot {
        Spot { row, column }
    }

    /// A selection reads as the text it holds: from its first cell to its
    /// last, whichever way it was dragged, a wide glyph with either of its
    /// columns in it, and each row less the blanks it ends in.
    #[test]
    fn a_selection_reads_as_the_text_from_its_first_cell_to_its_last() {
        let blank = || glyph(Shows::Blank);
        let view = view(vec![
            vec![narrow("a"), narrow("b"), glyph(Shows::Wide("日".to_owned())), blank()],
            vec![narrow("c"), blank(), blank(), blank(), blank()],
        ]);
        let screen = view.screen.as_ref().unwrap();
        let selection = |from, to| Selection { pane: 1, from, to };
        assert_eq!(text(screen, selection(spot(0, 1), spot(1, 0))), "b日\nc");
        assert_eq!(text(screen, selection(spot(1, 0), spot(0, 1))), "b日\nc");
        assert_eq!(text(screen, selection(spot(0, 3), spot(0, 3))), "日");
        assert_eq!(text(screen, selection(spot(0, 0), spot(0, 1))), "ab");
    }

    /// A wide glyph takes two columns of the frame drawn, as the server says,
    /// and the second shows nothing of its own; one cut by the area shows as
    /// nothing at all, and the area past the frame is blank.
    #[test]
    fn a_glyph_is_drawn_as_wide_as_the_server_says() {
        let wide = glyph(Shows::Wide("👍🏽".to_owned()));
        let view = view(vec![vec![narrow("a"), wide, narrow("b")]]);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        buffer[(5, 1)].set_symbol("x");
        view.draw(&mut buffer, Rect::new(1, 0, 5, 2));
        assert_eq!(buffer[(2, 0)].symbol(), "👍🏽");
        assert_eq!(buffer[(2, 0)].diff_option, CellDiffOption::ForcedWidth(side(2)));
        assert_eq!(buffer[(3, 0)].diff_option, CellDiffOption::Skip);
        assert_eq!(buffer[(4, 0)].symbol(), "b");
        assert_eq!(buffer[(5, 1)].symbol(), " ");
        let mut cut = Buffer::empty(Rect::new(0, 0, 2, 1));
        view.draw(&mut cut, Rect::new(0, 0, 2, 1));
        assert_eq!((cut[(0, 0)].symbol(), cut[(1, 0)].symbol()), ("a", " "));
    }

    /// A key that types a character goes by the one it types with no
    /// modifier, and says what it types with `Shift`; a named key types
    /// nothing; and one the server has no name for goes nowhere.
    #[test]
    fn a_key_goes_to_the_server_by_its_name_or_its_character() {
        let said = |code, modifiers| key_said(&KeyEvent::new(code, modifiers));
        let named = |named: api::Named| Some(api::key::Key::Named(named.into()));
        let shifted = said(KeyCode::Char('A'), KeyModifiers::SHIFT).unwrap();
        assert_eq!(shifted.key, Some(api::key::Key::Character('a'.into())));
        assert_eq!((shifted.text.as_str(), shifted.modifiers.unwrap().shift), ("A", true));
        let control = said(KeyCode::Char('c'), KeyModifiers::CONTROL).unwrap();
        assert_eq!((control.text.as_str(), control.modifiers.unwrap().ctrl), ("c", true));
        let back = said(KeyCode::BackTab, KeyModifiers::NONE).unwrap();
        assert_eq!(back.key, named(api::Named::Tab));
        assert_eq!((back.text.as_str(), back.modifiers.unwrap().shift), ("", true));
        assert_eq!(said(KeyCode::F(12), KeyModifiers::NONE).unwrap().key, named(api::Named::F12));
        assert_eq!(said(KeyCode::F(13), KeyModifiers::NONE), None);
        assert_eq!(said(KeyCode::Char('\u{7}'), KeyModifiers::NONE), None);
    }

    /// The left button selects where the program does not take the mouse, and
    /// with `Shift` held where it does; a selection copies only once it moved
    /// off the cell it went down on, and a frame of another size takes it
    /// away.
    #[test]
    fn a_drag_selects_where_the_program_does_not_take_the_mouse() {
        let press = |modifiers| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers,
        };
        assert!(selects(false, press(KeyModifiers::NONE)));
        assert!(!selects(true, press(KeyModifiers::NONE)));
        assert!(selects(true, press(KeyModifiers::SHIFT)));
        let mut view = view(vec![vec![narrow("a"), narrow("b"), narrow("c")]]);
        view.select_from(spot(0, 1));
        assert_eq!(view.selected(), None);
        view.select_from(spot(0, 0));
        view.select_to(spot(0, 9));
        assert_eq!(view.selected().as_deref(), Some("abc"));
        assert!(view.selection.is_some());
        let line = Line::new(vec![narrow("a"), narrow("b")], false, side(2)).unwrap();
        let rows = Rows::Whole(vec![line]);
        let size = Size { columns: side(2), rows: side(1) };
        let frame = Frame { pane: 1, size, rows, cursor: None, tracks_mouse: false };
        view.heard(&Heard::Frame(frame)).unwrap();
        assert_eq!(view.selection, None);
    }

    /// The rows that changed of a frame follow the last frame of its size: of
    /// another size, or with none before, they do not, and what the client
    /// had stays.
    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size() {
        let mut view = view(vec![vec![narrow("a"), narrow("b")]]);
        let line = Line::new(vec![narrow("c")], false, side(1)).unwrap();
        let rows = Rows::Changed(vec![(0, line.clone())]);
        let size = Size { columns: side(1), rows: side(1) };
        let frame = Frame { pane: 1, size, rows, cursor: None, tracks_mouse: false };
        let why = view.heard(&Heard::Frame(frame.clone())).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 2x1");
        assert_eq!(view.screen.as_ref().unwrap().lines[0].glyphs()[0], narrow("a"));
        view.screen = None;
        assert!(view.heard(&Heard::Frame(frame)).unwrap_err().ends_with("the rows of none"));
        assert!(view.screen.is_none());
    }
}
