use std::num::NonZeroU16;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind,
};
use lunetad::mux::own::api;
use lunetad::mux::own::frames::{self, Cursor, Glyph, Line, Shows, Size};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct At {
    pub(crate) row: u16,
    pub(crate) column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) from: At,
    pub(crate) to: At,
}

impl Selection {
    pub(crate) fn ends(self) -> (At, At) {
        if self.to < self.from {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        }
    }

    pub(crate) fn holds(self, at: At) -> bool {
        let (first, last) = self.ends();
        first <= at && at <= last
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pointer {
    Free,
    Sending,
    Pressed(At),
    Selecting(Selection),
    Shown(Selection),
}

impl Pointer {
    pub(crate) fn unselected(self) -> Pointer {
        match self {
            Pointer::Pressed(_) | Pointer::Selecting(_) | Pointer::Shown(_) => Pointer::Free,
            Pointer::Free | Pointer::Sending => self,
        }
    }

    pub(crate) fn selection(self) -> Option<Selection> {
        match self {
            Pointer::Pressed(at) => Some(Selection { from: at, to: at }),
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

pub(crate) fn spot_in(inner: Rect, at: Position) -> At {
    assert!(!inner.is_empty(), "a spot is on a screen of at least one cell");
    let row = at.y.saturating_sub(inner.y).min(inner.height - 1);
    let column = at.x.saturating_sub(inner.x).min(inner.width - 1);
    At { row, column }
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
    pointed: Option<Selection>,
) -> Option<Position> {
    for (row, line) in (0..area.height).zip(&screen.lines) {
        let mut column = 0;
        for glyph in line.glyphs() {
            if column >= area.width {
                break;
            }
            let at = At { row, column };
            let holds = |span: Option<Selection>| span.is_some_and(|span| span.holds(at));
            let mut lit = Modifier::empty();
            if holds(selection) {
                lit |= Modifier::REVERSED;
            }
            if holds(pointed) {
                lit |= Modifier::UNDERLINED;
            }
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

pub(crate) fn within(at: At, size: Size) -> At {
    At { row: at.row.min(size.rows.get() - 1), column: at.column.min(size.columns.get() - 1) }
}

fn paint(buffer: &mut Buffer, area: Rect, at: At, glyph: &Glyph, lit: Modifier) {
    let columns = glyph.shows.columns();
    let fits = at.column + columns <= area.width;
    let (x, y) = (area.x + at.column, area.y + at.row);
    let Some(cell) = buffer.cell_mut((x, y)) else {
        return;
    };
    let style = styled(&glyph.style).add_modifier(lit);
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

const LINK_CELLS_MAX: usize = 4096;

const URL_ENDS: &str = "<>\"'`";

const PATH_ENDS: &str = "()[]{}<>\"'`,;|=*";

const URL_TRAILS: &str = ".,;:!?";

const PATH_TRAILS: &str = ".:!?";

const PATH_LEADS: &str = ":!?";

const PAIRS: [(&str, &str); 3] = [("(", ")"), ("[", "]"), ("{", "}")];

pub(crate) fn link(screen: &Screen, at: At) -> Option<(Selection, String)> {
    let (word, clicked) = word(screen, at)?;
    let (first, end) = url(&word, clicked).or_else(|| path(&word, clicked))?;
    assert!(first <= clicked, "a link starts at or before the cell clicked");
    assert!(clicked < end, "a link ends after the cell clicked");
    let selection = Selection { from: word[first].at, to: word[end - 1].at };
    let text: String = word[first..end].iter().map(|letter| letter.text).collect();
    assert!(!text.is_empty(), "a link shows something");
    Some((selection, text))
}

#[derive(Clone, Copy)]
struct Letter<'a> {
    at: At,
    text: &'a str,
}

#[derive(Clone, Copy)]
struct Place {
    row: u16,
    index: usize,
    column: u16,
}

fn word(screen: &Screen, at: At) -> Option<(Vec<Letter<'_>>, usize)> {
    let line = screen.lines.get(usize::from(at.row))?;
    let place = under(line, at)?;
    let own = letter(screen, place)?;
    let before = letters(screen, place, left, LINK_CELLS_MAX - 1)?;
    let after = letters(screen, place, right, LINK_CELLS_MAX - 1 - before.len())?;
    let clicked = before.len();
    let mut word: Vec<Letter> = before.into_iter().rev().collect();
    word.push(own);
    word.extend(after);
    assert!(word.len() <= LINK_CELLS_MAX, "a word is cut at {LINK_CELLS_MAX} cells");
    assert!(word.windows(2).all(|pair| pair[0].at < pair[1].at), "a word reads forward");
    Some((word, clicked))
}

fn under(line: &Line, at: At) -> Option<Place> {
    let mut column = 0;
    for (index, glyph) in line.glyphs().iter().enumerate() {
        let columns = glyph.shows.columns();
        if at.column < column + columns {
            assert!(column <= at.column, "the glyph under a spot starts at or before it");
            return Some(Place { row: at.row, index, column });
        }
        column += columns;
    }
    None
}

fn letter(screen: &Screen, place: Place) -> Option<Letter<'_>> {
    let glyph = &screen.lines[usize::from(place.row)].glyphs()[place.index];
    let text = match &glyph.shows {
        Shows::Blank => return None,
        Shows::Narrow(grapheme) | Shows::Wide(grapheme) => grapheme.as_str(),
    };
    if text.chars().all(char::is_whitespace) {
        return None;
    }
    Some(Letter { at: At { row: place.row, column: place.column }, text })
}

fn letters(
    screen: &Screen,
    mut place: Place,
    step: fn(&Screen, Place) -> Option<Place>,
    count_max: usize,
) -> Option<Vec<Letter<'_>>> {
    let mut letters = Vec::new();
    for _ in 0..=count_max {
        let Some(next) = step(screen, place) else {
            return Some(letters);
        };
        let Some(found) = letter(screen, next) else {
            return Some(letters);
        };
        if letters.len() == count_max {
            return None;
        }
        letters.push(found);
        place = next;
    }
    unreachable!("a walk ends within {count_max} letters")
}

fn left(screen: &Screen, place: Place) -> Option<Place> {
    if place.index > 0 {
        let glyphs = screen.lines[usize::from(place.row)].glyphs();
        let index = place.index - 1;
        let column = place.column.checked_sub(glyphs[index].shows.columns());
        let column = column.expect("a glyph starts after the glyphs before it");
        return Some(Place { row: place.row, index, column });
    }
    let row = place.row.checked_sub(1)?;
    let line = &screen.lines[usize::from(row)];
    if line.wraps() {
        let index = line.glyphs().len().checked_sub(1)?;
        let column = line.glyphs()[..index].iter().map(|glyph| glyph.shows.columns()).sum();
        Some(Place { row, index, column })
    } else {
        None
    }
}

fn right(screen: &Screen, place: Place) -> Option<Place> {
    let line = &screen.lines[usize::from(place.row)];
    let index = place.index + 1;
    if index < line.glyphs().len() {
        let column = place.column + line.glyphs()[place.index].shows.columns();
        return Some(Place { row: place.row, index, column });
    }
    if line.wraps() {
        let row = place.row.checked_add(1)?;
        let below = screen.lines.get(usize::from(row))?;
        below.glyphs().first()?;
        Some(Place { row, index: 0, column: 0 })
    } else {
        None
    }
}

fn one_of(text: &str, set: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(|first| set.contains(first)) && characters.next().is_none()
}

fn url(word: &[Letter], clicked: usize) -> Option<(usize, usize)> {
    assert!(clicked < word.len(), "the cell clicked is in its word");
    let text = |at: usize| word[at].text;
    let scheme = |at: usize| {
        let text = text(at);
        text.len() == 1 && text.chars().all(|it| it.is_ascii_alphanumeric() || "+.-".contains(it))
    };
    let marked = |at: usize| {
        let three = word.get(at..at + 3).map(|three| three.iter().map(|letter| letter.text));
        three.is_some_and(|three| three.eq([":", "/", "/"]))
    };
    let mut scanned = 0;
    for mark in 0..word.len() {
        if mark < scanned {
            continue;
        }
        if !marked(mark) {
            continue;
        }
        let mut first = mark;
        while first > 0 && scheme(first - 1) {
            first -= 1;
        }
        while first < mark && !text(first).chars().all(|it| it.is_ascii_alphabetic()) {
            first += 1;
        }
        if first == mark {
            continue;
        }
        if clicked < first {
            return None;
        }
        let after = mark + 3;
        let mut end = after;
        while end < word.len() && !one_of(text(end), URL_ENDS) {
            end += 1;
        }
        scanned = end;
        let end = untrailed(word, after, end);
        if after == end {
            continue;
        }
        if clicked < end {
            return Some((first, end));
        }
    }
    None
}

fn untrailed(word: &[Letter], from: usize, end: usize) -> usize {
    assert!(from <= end, "a url's tail starts before it ends");
    assert!(end <= word.len(), "a url ends within its word");
    let mut opens = [0_usize; PAIRS.len()];
    let mut closes = [0_usize; PAIRS.len()];
    for letter in &word[from..end] {
        for (pair, (open, close)) in PAIRS.iter().enumerate() {
            opens[pair] += usize::from(letter.text == *open);
            closes[pair] += usize::from(letter.text == *close);
        }
    }
    let mut kept = end;
    while kept > from {
        let last = word[kept - 1].text;
        let closing = PAIRS.iter().position(|(_, close)| last == *close);
        let unopened = closing.is_some_and(|pair| opens[pair] < closes[pair]);
        if one_of(last, URL_TRAILS) || unopened {
            if let Some(pair) = closing {
                closes[pair] -= 1;
            }
            kept -= 1;
        } else {
            break;
        }
    }
    assert!(from <= kept, "a url keeps what follows its scheme, or none of it");
    assert!(kept <= end, "a url only loses its tail");
    kept
}

fn path(word: &[Letter], clicked: usize) -> Option<(usize, usize)> {
    assert!(clicked < word.len(), "the cell clicked is in its word");
    let ends = |at: usize| one_of(word[at].text, PATH_ENDS);
    if ends(clicked) {
        return None;
    }
    let mut first = clicked;
    while first > 0 && !ends(first - 1) {
        first -= 1;
    }
    let mut end = clicked + 1;
    while end < word.len() && !ends(end) {
        end += 1;
    }
    while first < end && one_of(word[first].text, PATH_LEADS) {
        first += 1;
    }
    while end > first && one_of(word[end - 1].text, PATH_TRAILS) {
        end -= 1;
    }
    let held = &word[first..end];
    let holds_click = (first..end).contains(&clicked);
    let folders = held.iter().any(|letter| letter.text == "/");
    let named = held.iter().any(|letter| letter.text.chars().any(char::is_alphanumeric));
    (holds_click && folders && named).then_some((first, end))
}

fn styled(style: &frames::Style) -> Style {
    let colour = |colour: Option<frames::Colour>| match colour {
        Some(frames::Colour::Palette(index)) => Color::Indexed(index),
        Some(frames::Colour::Rgb([red, green, blue])) => Color::Rgb(red, green, blue),
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

pub(crate) fn mouse_said(mouse: MouseEvent, at: At) -> Option<api::Mouse> {
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
    use lunetad::mux::own::frames::{Ending, Grapheme};

    use super::*;

    fn glyph(shows: Shows) -> Glyph {
        Glyph { shows, style: frames::Style::default() }
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

    fn ended(glyphs: Vec<Glyph>, ending: Ending) -> Line {
        let columns: u16 = glyphs.iter().map(|glyph| glyph.shows.columns()).sum();
        let columns = columns + u16::from(ending == Ending::WrapsWide);
        Line::new(glyphs, ending, side(columns)).unwrap()
    }

    fn line(glyphs: Vec<Glyph>) -> Line {
        ended(glyphs, Ending::Here)
    }

    fn spot(row: u16, column: u16) -> At {
        At { row, column }
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

    fn typed_ending(rows: &[&str], ending: impl Fn(usize) -> Ending) -> Screen {
        let cell = |it: char| if it == ' ' { glyph(Shows::Blank) } else { narrow(&it.to_string()) };
        let lines = rows.iter().enumerate();
        let lines = lines.map(|(index, row)| ended(row.chars().map(cell).collect(), ending(index)));
        let columns = side(u16::try_from(rows[0].chars().count()).unwrap());
        let size = Size { columns, rows: side(u16::try_from(rows.len()).unwrap()) };
        Screen { size, lines: lines.collect(), cursor: None, tracks_mouse: false }
    }

    fn typed(rows: &[&str]) -> Screen {
        typed_ending(rows, |_| Ending::Here)
    }

    fn wrapping(rows: &[&str]) -> Screen {
        let last = rows.len() - 1;
        typed_ending(rows, |index| if index < last { Ending::Wraps } else { Ending::Here })
    }

    fn clicked(rows: &[&str], row: u16, column: u16) -> Option<(At, At, String)> {
        let (span, text) = link(&typed(rows), spot(row, column))?;
        Some((span.from, span.to, text))
    }

    #[test]
    fn a_click_takes_the_whole_url_it_lands_on_and_not_the_punctuation_around_it() {
        let said = "see (https://e.io/a_(b)?q=1#x), ok.";
        let url = Some((spot(0, 5), spot(0, 28), "https://e.io/a_(b)?q=1#x".to_owned()));
        assert_eq!(clicked(&[said], 0, 5), url);
        assert_eq!(clicked(&[said], 0, 16), url);
        assert_eq!(clicked(&[said], 0, 28), url);
        assert_eq!(clicked(&[said], 0, 29), None, "the bracket closes what the url is in");
        assert_eq!(clicked(&[said], 0, 4), None);
        assert_eq!(clicked(&[said], 0, 1), None, "a word is not a link");
        assert_eq!(clicked(&[said], 0, 3), None, "nor is a blank");
        let quoted = r#"at "git+ssh://h/r.git"."#;
        assert_eq!(clicked(&[quoted], 0, 9).unwrap().2, "git+ssh://h/r.git");
        assert_eq!(clicked(&["[doc](http://x.y/z)"], 0, 8).unwrap().2, "http://x.y/z");
        let nested = "https://a.io/?u=http://b";
        assert_eq!(clicked(&[nested], 0, 20).unwrap().2, nested, "a url in a url is the outer one");
        assert_eq!(clicked(&["://x"], 0, 2).unwrap().2, "//x", "with no scheme it is a path");
    }

    #[test]
    fn a_click_takes_the_file_path_it_lands_on_with_its_line() {
        let said = "Update(crates/a.rs) ~/b/c.txt: src/m.rs:12:5. and/or";
        let path = Some((spot(0, 7), spot(0, 17), "crates/a.rs".to_owned()));
        assert_eq!(clicked(&[said], 0, 9), path);
        assert_eq!(clicked(&[said], 0, 2), None, "the call is no path");
        assert_eq!(clicked(&[said], 0, 21).unwrap().2, "~/b/c.txt");
        assert_eq!(clicked(&[said], 0, 33).unwrap().2, "src/m.rs:12:5");
        assert_eq!(clicked(&[said], 0, 46).unwrap().2, "and/or", "what has a slash may be a path");
        assert_eq!(clicked(&["a / b"], 0, 2), None, "a slash alone names nothing");
        assert_eq!(clicked(&["../up"], 0, 0).unwrap().2, "../up");
        assert_eq!(clicked(&["README.md"], 0, 0), None, "a name with no folder is a word");
    }

    #[test]
    fn a_link_the_program_wraps_goes_on_in_the_next_row() {
        let rows = ["go https://e.", "io/a/b now   ", "x            "];
        let url = Some((spot(0, 3), spot(1, 5), "https://e.io/a/b".to_owned()));
        let wrapped = |row, column| {
            let (span, text) = link(&wrapping(&rows), spot(row, column))?;
            Some((span.from, span.to, text))
        };
        assert_eq!(wrapped(0, 4), url);
        assert_eq!(wrapped(1, 1), url);
        assert_eq!(wrapped(1, 8), None, "a row that ends in a blank ends its word");
        let cut = clicked(&rows[..2], 0, 4).unwrap().2;
        assert_eq!(cut, "https://e", "a row the program ended at the edge ends its word");
        let wide = || glyph(Shows::Wide(grapheme("日")));
        let blank = || glyph(Shows::Blank);
        let screen = |columns: u16, lines: Vec<Line>| {
            let size = Size { columns: side(columns), rows: side(2) };
            Screen { size, lines, cursor: None, tracks_mouse: false }
        };
        let ends_wide = screen(
            5,
            vec![
                ended(vec![narrow("a"), narrow("b"), narrow("/"), wide()], Ending::Wraps),
                line(vec![narrow("x"), narrow("/"), narrow("y"), blank(), blank()]),
            ],
        );
        assert_eq!(link(&ends_wide, spot(0, 4)).unwrap().1, "ab/日x/y", "either half of it");
        assert_eq!(link(&ends_wide, spot(1, 0)).unwrap().1, "ab/日x/y", "from either row");
        let early = ended(vec![narrow("a"), narrow("b"), narrow("/")], Ending::WrapsWide);
        let wrapped = screen(4, vec![early, line(vec![wide(), narrow("y"), blank()])]);
        let (span, text) = link(&wrapped, spot(0, 0)).unwrap();
        assert_eq!((span.to, text.as_str()), (spot(1, 2), "ab/日y"), "a wide glyph wrapped early");
        assert_eq!(link(&wrapped, spot(0, 3)), None, "the padding it left shows nothing");
    }

    #[test]
    fn a_word_longer_than_any_link_is_none() {
        let row = "a/".repeat(32);
        let full = vec![row.as_str(); LINK_CELLS_MAX / row.len()];
        let longest = link(&wrapping(&full), spot(10, 3)).unwrap().1;
        assert_eq!(longest.len(), LINK_CELLS_MAX);
        let mut longer = full.clone();
        let last = format!("a{}", " ".repeat(63));
        longer.push(last.as_str());
        assert_eq!(link(&wrapping(&longer), spot(10, 3)), None);
        let marks = "://".repeat(21) + "/";
        let marks = wrapping(&vec![marks.as_str(); 64]);
        assert_eq!(link(&marks, spot(63, 63)), None, "marks with no scheme and no name");
        let closers = format!("a://{}", ")".repeat(60));
        let mut rows = vec![closers.as_str()];
        let tail = ")".repeat(64);
        rows.extend(vec![tail.as_str(); 62]);
        assert_eq!(link(&wrapping(&rows), spot(30, 5)), None, "a scheme and only closers after it");
    }

    #[test]
    fn a_glyph_is_drawn_as_wide_as_the_server_says() {
        let wide = glyph(Shows::Wide(grapheme("👍🏽")));
        let rows = vec![line(vec![narrow("a"), wide, narrow("b")])];
        let size = Size { columns: side(4), rows: side(1) };
        let screen = Screen { size, lines: rows, cursor: None, tracks_mouse: false };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        draw_screen(&mut buffer, Rect::new(1, 0, 5, 1), &screen, None, None);
        assert_eq!(buffer[(2, 0)].symbol(), "👍🏽");
        assert_eq!(buffer[(2, 0)].diff_option, CellDiffOption::ForcedWidth(side(2)));
        assert_eq!(buffer[(3, 0)].diff_option, CellDiffOption::Skip);
        assert_eq!(buffer[(4, 0)].symbol(), "b");
        let mut cut = Buffer::empty(Rect::new(0, 0, 2, 1));
        draw_screen(&mut cut, Rect::new(0, 0, 2, 1), &screen, None, None);
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
