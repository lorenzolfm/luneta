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

struct Screen {
    size: Size,
    lines: Vec<Line>,
    cursor: Option<Cursor>,
    tracks_mouse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spot {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    from: Spot,
    to: Spot,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Selected {
    #[default]
    Nothing,
    Dragging(Selection),
    Held(Selection),
}

impl Selection {
    fn ends(self) -> (Spot, Spot) {
        if self.to < self.from {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        }
    }

    fn holds(self, spot: Spot) -> bool {
        let (first, last) = self.ends();
        first <= spot && spot <= last
    }
}

#[derive(Default)]
struct View {
    went: Option<Went>,
    screen: Option<Screen>,
    selected: Selected,
}

impl View {
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
            Heard::Stayed(_) | Heard::Ended | Heard::Lost(_) => {},
        }
        Ok(())
    }

    fn framed(&mut self, frame: &Frame) -> Result<(), String> {
        if self.went.is_none_or(|went| went.pane.id.get() != frame.pane()) {
            return Ok(());
        }
        let before = self.screen.take();
        let size = before.as_ref().map(|screen| screen.size);
        let lines = match (frame.rows(), before) {
            (Rows::Whole(lines), _) => lines.clone(),
            (Rows::Changed(rows), Some(screen)) if screen.size == frame.size() => {
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
                let sides = sides(frame.size());
                return Err(format!("a frame of {sides} changes the rows of {had}"));
            },
        };
        if size != Some(frame.size()) {
            self.unselect();
        }
        self.screen = Some(Screen {
            size: frame.size(),
            lines,
            cursor: frame.cursor(),
            tracks_mouse: frame.tracks_mouse(),
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
        self.selected = Selected::Dragging(Selection { from: at, to: at });
    }

    fn select_to(&mut self, at: Spot) {
        let (Some(screen), Selected::Dragging(selection)) = (&self.screen, &mut self.selected)
        else {
            return;
        };
        selection.to = within(at, screen.size);
    }

    fn selected(&mut self) -> Option<String> {
        let Selected::Dragging(selection) = self.selected else {
            return None;
        };
        let moved = Some(selection).filter(|selection| selection.from != selection.to);
        let text = moved.and_then(|selection| Some(text(self.screen.as_ref()?, selection)));
        let text = text.filter(|text| !text.is_empty());
        self.selected = match text {
            Some(_) => Selected::Held(selection),
            None => Selected::Nothing,
        };
        text
    }

    fn unselect(&mut self) {
        self.selected = Selected::Nothing;
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
        let selection = match self.selected {
            Selected::Nothing => None,
            Selected::Dragging(selection) | Selected::Held(selection) => Some(selection),
        };
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

pub struct Attached {
    attach: Attach,
    number: u64,
    size: Size,
    view: View,
}

impl Attached {
    pub fn start(number: u64, hello: Hello, events: &Sender<Event>) -> Attached {
        let events = events.clone();
        let attach = attach::attach(hello, move |heard| {
            let _ = events.send(Event::Own(number, heard));
        });
        Attached { attach, number, size: hello.size, view: View::default() }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn heard(&mut self, heard: &Heard) {
        if let Err(why) = self.view.heard(heard) {
            self.attach.give_up(format!("luneta-mux said what luneta cannot follow: {why}"));
        }
    }

    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) {
        let size = Size::within(columns, rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.attach.send(to_server::Event::Resize(size.said()));
    }

    pub fn key(&mut self, key: &KeyEvent) {
        self.view.unselect();
        if let Some(key) = key_said(key) {
            self.attach.send(to_server::Event::Key(key));
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.view.unselect();
        self.attach.send(to_server::Event::Paste(api::Paste { text: text.to_owned() }));
    }

    pub fn focus(&self, focused: bool) {
        self.attach.send(to_server::Event::Focus(api::Focus { focused }));
    }

    pub fn go(&self, to: To) {
        self.attach.go(to);
    }

    pub fn mouse(&self, mouse: MouseEvent, at: Spot) {
        if let Some(mouse) = mouse_said(mouse, at) {
            self.attach.send(to_server::Event::Mouse(mouse));
        }
    }

    pub fn selects(&self, mouse: MouseEvent) -> bool {
        selects(self.view.tracks_mouse(), mouse)
    }

    pub fn select_from(&mut self, at: Spot) {
        self.view.select_from(at);
    }

    pub fn select_to(&mut self, at: Spot) {
        self.view.select_to(at);
    }

    pub fn selected(&mut self) -> Option<String> {
        self.view.selected()
    }

    pub fn unselect(&mut self) {
        self.view.unselect();
    }

    pub fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        self.view.draw(buffer, area)
    }

    pub fn cursor_style(&self) -> (CursorShape, bool) {
        self.view.cursor_style()
    }
}

fn selects(tracks: bool, mouse: MouseEvent) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && (!tracks || mouse.modifiers.contains(KeyModifiers::SHIFT))
}

fn within(at: Spot, size: Size) -> Spot {
    Spot { row: at.row.min(size.rows.get() - 1), column: at.column.min(size.columns.get() - 1) }
}

fn paint(buffer: &mut Buffer, area: Rect, at: Spot, glyph: &Glyph, lit: bool) {
    let columns = glyph.shows.columns();
    let fits = at.column + columns <= area.width;
    let (x, y) = (area.x + at.column, area.y + at.row);
    let Some(cell) = buffer.cell_mut((x, y)) else {
        return;
    };
    let mut style = styled(&glyph.style);
    if lit {
        style = style.add_modifier(Modifier::REVERSED);
    }
    cell.set_style(style);
    match &glyph.shows {
        Shows::Narrow(grapheme) => {
            cell.set_symbol(grapheme.as_str());
            cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        },
        Shows::Wide(grapheme) if fits => {
            cell.set_symbol(grapheme.as_str());
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
                    Shows::Narrow(grapheme) | Shows::Wide(grapheme) => {
                        said.push_str(grapheme.as_str());
                    },
                }
            }
            column = end + 1;
        }
        rows.push(said.trim_end().to_owned());
    }
    rows.join("\n")
}

fn styled(style: &attach::Style) -> Style {
    let colour = |colour: Option<attach::Colour>| match colour {
        Some(attach::Colour::Palette(index)) => Color::Indexed(index),
        Some(attach::Colour::Rgb([red, green, blue])) => Color::Rgb(red, green, blue),
        None => Color::Reset,
    };
    let mut drawn = Style::new().fg(colour(style.foreground)).bg(colour(style.background));
    if let Some(underline) = style.underline_colour {
        drawn = drawn.underline_color(colour(Some(underline)));
    }
    let modifiers = [
        (style.bold, Modifier::BOLD),
        (style.faint, Modifier::DIM),
        (style.italic, Modifier::ITALIC),
        (style.underlined, Modifier::UNDERLINED),
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

fn modifiers_said(modifiers: KeyModifiers) -> api::Modifiers {
    api::Modifiers {
        shift: modifiers.contains(KeyModifiers::SHIFT),
        alt: modifiers.contains(KeyModifiers::ALT),
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        super_key: modifiers.contains(KeyModifiers::SUPER),
    }
}

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
    use std::num::NonZeroU32;

    use lunetad::mux::own::attach::Grapheme;
    use lunetad::mux::own::{OwnClientId, OwnId, OwnKey, OwnMade, OwnPane};

    use super::*;

    fn glyph(shows: Shows) -> Glyph {
        Glyph { shows, style: attach::Style::default() }
    }

    fn grapheme(grapheme: &str) -> Grapheme {
        Grapheme::new(grapheme.to_owned()).unwrap()
    }

    fn narrow(it: &str) -> Glyph {
        glyph(Shows::Narrow(grapheme(it)))
    }

    fn side(count: u16) -> NonZeroU16 {
        NonZeroU16::new(count).unwrap()
    }

    fn view(rows: Vec<Vec<Glyph>>) -> View {
        let columns = rows[0].iter().map(|glyph| glyph.shows.columns()).sum();
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|glyphs| Line::new(glyphs, false, side(columns)).unwrap())
            .collect();
        let size = Size { columns: side(columns), rows: side(u16::try_from(lines.len()).unwrap()) };
        let screen = Screen { size, lines, cursor: None, tracks_mouse: false };
        let session = OwnKey { id: OwnId::new(1).unwrap(), made: OwnMade::from_nanos(0) };
        let (pane, client) = (OwnPane::new(session, 1).unwrap(), OwnClientId::new(1).unwrap());
        let went = Went { pane, tab: NonZeroU32::MIN, client };
        View { went: Some(went), screen: Some(screen), ..View::default() }
    }

    fn spot(row: u16, column: u16) -> Spot {
        Spot { row, column }
    }

    #[test]
    fn a_selection_reads_as_the_text_from_its_first_cell_to_its_last() {
        let blank = || glyph(Shows::Blank);
        let view = view(vec![
            vec![narrow("a"), narrow("b"), glyph(Shows::Wide(grapheme("日"))), blank()],
            vec![narrow("c"), blank(), blank(), blank(), blank()],
        ]);
        let screen = view.screen.as_ref().unwrap();
        let selection = |from, to| Selection { from, to };
        assert_eq!(text(screen, selection(spot(0, 1), spot(1, 0))), "b日\nc");
        assert_eq!(text(screen, selection(spot(1, 0), spot(0, 1))), "b日\nc");
        assert_eq!(text(screen, selection(spot(0, 3), spot(0, 3))), "日");
        assert_eq!(text(screen, selection(spot(0, 0), spot(0, 1))), "ab");
    }

    #[test]
    fn a_glyph_is_drawn_as_wide_as_the_server_says() {
        let wide = glyph(Shows::Wide(grapheme("👍🏽")));
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
        assert!(matches!(view.selected, Selected::Held(_)));
        let line = Line::new(vec![narrow("a"), narrow("b")], false, side(2)).unwrap();
        let rows = Rows::Whole(vec![line]);
        let size = Size { columns: side(2), rows: side(1) };
        let frame = Frame::new(1, size, rows, None, false).unwrap();
        view.heard(&Heard::Frame(frame)).unwrap();
        assert_eq!(view.selected, Selected::Nothing);
    }

    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size() {
        let mut view = view(vec![vec![narrow("a"), narrow("b")]]);
        let line = Line::new(vec![narrow("c")], false, side(1)).unwrap();
        let rows = Rows::Changed(vec![(0, line.clone())]);
        let size = Size { columns: side(1), rows: side(1) };
        let frame = Frame::new(1, size, rows, None, false).unwrap();
        let why = view.heard(&Heard::Frame(frame.clone())).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 2x1");
        assert_eq!(view.screen.as_ref().unwrap().lines[0].glyphs()[0], narrow("a"));
        view.screen = None;
        assert!(view.heard(&Heard::Frame(frame)).unwrap_err().ends_with("the rows of none"));
        assert!(view.screen.is_none());
    }
}
