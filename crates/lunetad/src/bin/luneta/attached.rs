use std::num::NonZeroU16;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind,
};
use lunetad::mux::own::api;
use lunetad::mux::own::attach::{self, Cursor, Glyph, Line, Shows, Size};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use unicode_width::UnicodeWidthStr as _;

use crate::gruvbox;

pub(crate) struct Screen {
    pub(crate) size: Size,
    pub(crate) lines: Vec<Line>,
    pub(crate) cursor: Option<Cursor>,
    pub(crate) tracks_mouse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spot {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) from: Spot,
    pub(crate) to: Spot,
}

impl Selection {
    pub(crate) fn ends(self) -> (Spot, Spot) {
        if self.to < self.from {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        }
    }

    pub(crate) fn holds(self, spot: Spot) -> bool {
        let (first, last) = self.ends();
        first <= spot && spot <= last
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pointer {
    Free,
    Sending,
    Selecting(Selection),
    Shown(Selection),
}

impl Pointer {
    pub(crate) fn unselected(self) -> Pointer {
        match self {
            Pointer::Selecting(_) | Pointer::Shown(_) => Pointer::Free,
            Pointer::Free | Pointer::Sending => self,
        }
    }

    pub(crate) fn selection(self) -> Option<Selection> {
        match self {
            Pointer::Selecting(selection) | Pointer::Shown(selection) => Some(selection),
            Pointer::Free | Pointer::Sending => None,
        }
    }
}

pub(crate) const TAB_BAR_ROWS: u16 = 1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TabSpan<T> {
    pub(crate) id: T,
    pub(crate) column: u16,
    pub(crate) columns: u16,
    pub(crate) name: String,
}

pub(crate) fn on_panes(at: Spot) -> Option<Position> {
    Some(Position::new(at.column, at.row.checked_sub(TAB_BAR_ROWS)?))
}

pub(crate) fn below_bar(at: Spot) -> Position {
    Position::new(at.column, at.row.saturating_sub(TAB_BAR_ROWS))
}

pub(crate) fn screen_rect(outer: Rect) -> Rect {
    outer.inner(Margin::new(1, 1))
}

pub(crate) fn spot_in(inner: Rect, at: Position) -> Spot {
    assert!(!inner.is_empty(), "a spot is on a screen of at least one cell");
    let row = at.y.saturating_sub(inner.y).min(inner.height - 1);
    let column = at.x.saturating_sub(inner.x).min(inner.width - 1);
    Spot { row, column }
}

pub(crate) fn span_under<T: Copy>(spans: &[TabSpan<T>], column: u16) -> Option<T> {
    let under = |span: &&TabSpan<T>| (span.column..span.column + span.columns).contains(&column);
    spans.iter().find(under).map(|span| span.id)
}

pub(crate) fn printable(text: &str) -> String {
    text.chars().filter(|character| !character.is_control()).collect()
}

pub(crate) fn tab_spans<'a, T: Copy>(
    tabs: impl IntoIterator<Item = (T, &'a str)>,
    width: u16,
) -> Vec<TabSpan<T>> {
    let mut spans = Vec::new();
    let mut column: u16 = 0;
    for (id, name) in tabs {
        let name = printable(name);
        let wanted = u16::try_from(name.width()).unwrap_or(u16::MAX).saturating_add(2);
        let columns = wanted.min(width.saturating_sub(column));
        if columns == 0 {
            break;
        }
        spans.push(TabSpan { id, column, columns, name });
        column = column.saturating_add(columns).saturating_add(1);
    }
    let end = |span: &TabSpan<T>| u32::from(span.column) + u32::from(span.columns);
    for pair in spans.windows(2) {
        assert!(end(&pair[0]) < u32::from(pair[1].column), "tab spans keep a gap between them");
    }
    if let Some(last) = spans.last() {
        assert!(end(last) <= u32::from(width), "tab spans fit the bar");
    }
    spans
}

pub(crate) fn draw_tab_spans<T: Copy + PartialEq>(
    buffer: &mut Buffer,
    bar: Rect,
    spans: &[TabSpan<T>],
    current: T,
) {
    if bar.is_empty() {
        return;
    }
    buffer.set_style(bar, Style::new().bg(gruvbox::BG1));
    for span in spans {
        let style = if span.id == current {
            Style::new().fg(gruvbox::BG0).bg(gruvbox::GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(gruvbox::FG1).bg(gruvbox::BG2)
        };
        let x = bar.x + span.column;
        buffer.set_style(Rect::new(x, bar.y, span.columns, 1), style);
        let room = usize::from(span.columns.saturating_sub(2));
        buffer.set_stringn(x + 1, bar.y, &span.name, room, style);
    }
}

pub(crate) fn border(buffer: &mut Buffer, panes: Rect, outer: Rect, title: &str, style: Style) {
    if outer.is_empty() {
        return;
    }
    let set = border::ROUNDED;
    let (left, top) = (outer.left(), outer.top());
    let (right, bottom) = (outer.right() - 1, outer.bottom() - 1);
    let mut put = |x: u16, y: u16, symbol: &str| {
        if panes.contains(Position::new(x, y))
            && let Some(cell) = buffer.cell_mut((x, y))
        {
            cell.reset();
            cell.set_symbol(symbol).set_style(style);
        }
    };
    for x in left..=right {
        put(x, top, set.horizontal_top);
        put(x, bottom, set.horizontal_bottom);
    }
    for y in top..=bottom {
        put(left, y, set.vertical_left);
        put(right, y, set.vertical_right);
    }
    put(left, top, set.top_left);
    put(right, top, set.top_right);
    put(left, bottom, set.bottom_left);
    put(right, bottom, set.bottom_right);
    let title = printable(title);
    let (start, end) = (left + 1, right.min(panes.right()));
    if !title.is_empty() && start < end && panes.contains(Position::new(start, top)) {
        buffer.set_stringn(start, top, format!(" {title} "), usize::from(end - start), style);
    }
}

pub(crate) fn draw_screen(
    buffer: &mut Buffer,
    area: Rect,
    screen: &Screen,
    selection: Option<Selection>,
) -> Option<Position> {
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

pub(crate) fn selects(tracks: bool, mouse: MouseEvent) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && (!tracks || mouse.modifiers.contains(KeyModifiers::SHIFT))
}

pub(crate) fn within(at: Spot, size: Size) -> Spot {
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

pub(crate) fn text(screen: &Screen, selection: Selection) -> String {
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

pub(crate) fn key_said(key: &KeyEvent) -> Option<api::Key> {
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

pub(crate) fn mouse_said(mouse: MouseEvent, at: Spot) -> Option<api::Mouse> {
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
    use lunetad::mux::own::attach::Grapheme;

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

    fn line(glyphs: Vec<Glyph>) -> Line {
        let columns = glyphs.iter().map(|glyph| glyph.shows.columns()).sum();
        Line::new(glyphs, false, side(columns)).unwrap()
    }

    fn spot(row: u16, column: u16) -> Spot {
        Spot { row, column }
    }

    #[test]
    fn a_selection_reads_as_the_text_from_its_first_cell_to_its_last() {
        let blank = || glyph(Shows::Blank);
        let rows = vec![
            line(vec![narrow("a"), narrow("b"), glyph(Shows::Wide(grapheme("日"))), blank()]),
            line(vec![narrow("c"), blank(), blank(), blank(), blank()]),
        ];
        let size = Size { columns: side(5), rows: side(2) };
        let screen = Screen { size, lines: rows, cursor: None, tracks_mouse: false };
        let selection = |from, to| Selection { from, to };
        assert_eq!(text(&screen, selection(spot(0, 1), spot(1, 0))), "b日\nc");
        assert_eq!(text(&screen, selection(spot(1, 0), spot(0, 1))), "b日\nc");
        assert_eq!(text(&screen, selection(spot(0, 3), spot(0, 3))), "日");
        assert_eq!(text(&screen, selection(spot(0, 0), spot(0, 1))), "ab");
    }

    #[test]
    fn a_glyph_is_drawn_as_wide_as_the_server_says() {
        let wide = glyph(Shows::Wide(grapheme("👍🏽")));
        let rows = vec![line(vec![narrow("a"), wide, narrow("b")])];
        let size = Size { columns: side(4), rows: side(1) };
        let screen = Screen { size, lines: rows, cursor: None, tracks_mouse: false };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        draw_screen(&mut buffer, Rect::new(1, 0, 5, 1), &screen, None);
        assert_eq!(buffer[(2, 0)].symbol(), "👍🏽");
        assert_eq!(buffer[(2, 0)].diff_option, CellDiffOption::ForcedWidth(side(2)));
        assert_eq!(buffer[(3, 0)].diff_option, CellDiffOption::Skip);
        assert_eq!(buffer[(4, 0)].symbol(), "b");
        let mut cut = Buffer::empty(Rect::new(0, 0, 2, 1));
        draw_screen(&mut cut, Rect::new(0, 0, 2, 1), &screen, None);
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
}
