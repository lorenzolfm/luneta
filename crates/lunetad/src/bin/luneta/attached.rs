use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::mpsc::Sender;

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind,
};
use lunetad::mux::own::api::{self, to_server};
use lunetad::mux::own::{OwnKey, OwnPane};
use lunetad::mux::own::attach::{
    self, Attach, Cursor, Frame, Glyph, Heard, Hello, Layout, Line, Rows, Shows, Size, Tabbed,
    To, Went,
};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use unicode_width::UnicodeWidthStr as _;

use crate::gruvbox;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointer {
    Free,
    Sending,
    Selecting(Selection),
    Shown(Selection),
}

impl Pointer {
    fn unselected(self) -> Pointer {
        match self {
            Pointer::Selecting(_) | Pointer::Shown(_) => Pointer::Free,
            Pointer::Free | Pointer::Sending => self,
        }
    }

    fn selection(self) -> Option<Selection> {
        match self {
            Pointer::Selecting(selection) | Pointer::Shown(selection) => Some(selection),
            Pointer::Free | Pointer::Sending => None,
        }
    }
}

const TAB_BAR_ROWS: u16 = 1;

#[must_use]
pub fn laid_size(columns: NonZeroU16, rows: NonZeroU16) -> Size {
    let rows = NonZeroU16::new(rows.get().saturating_sub(TAB_BAR_ROWS)).unwrap_or(NonZeroU16::MIN);
    Size::within(columns, rows)
}

#[derive(Debug, PartialEq, Eq)]
enum Moused {
    Nothing,
    Go(To),
    Sent(api::Mouse),
}

#[derive(Debug, PartialEq, Eq)]
struct TabSpan {
    id: NonZeroU32,
    column: u16,
    columns: u16,
    name: String,
}

#[derive(Default)]
enum View {
    #[default]
    Unseated,
    Going {
        went: Went,
        kept: Option<Kept>,
    },
    Laid {
        went: Went,
        kept: Kept,
    },
}

struct Kept {
    layout: Layout,
    screens: BTreeMap<NonZeroU32, Screen>,
    pointer: Pointer,
}

impl View {
    fn heard(&mut self, heard: &Heard) -> Result<(), String> {
        match heard {
            Heard::Went(went) => self.went(*went),
            Heard::Layout(layout) => self.laid(layout)?,
            Heard::Frame(frame) => self.framed(frame)?,
            Heard::Stayed(_) | Heard::Refused(_) | Heard::Ended | Heard::Lost(_) => {},
        }
        Ok(())
    }

    fn went(&mut self, went: Went) {
        let (before, kept) = match std::mem::take(self) {
            View::Unseated => (None, None),
            View::Going { went, kept } => (Some(went), kept),
            View::Laid { went, kept } => (Some(went), Some(kept)),
        };
        let tab = |went: Went| (went.pane.session, went.tab);
        let mut kept = kept.filter(|_| before.is_some_and(|before| tab(before) == tab(went)));
        if let Some(kept) = &mut kept
            && before.is_some_and(|before| before.pane != went.pane)
        {
            kept.pointer = Pointer::Free;
        }
        *self = View::Going { went, kept };
    }

    fn laid(&mut self, layout: &Layout) -> Result<(), String> {
        let went = match self {
            View::Unseated => {
                return Err(format!("a layout of tab {} came before a went", layout.tab()));
            },
            View::Going { went, .. } | View::Laid { went, .. } => *went,
        };
        if layout.tab() != went.tab {
            return Err(format!("a layout of tab {} came on tab {}", layout.tab(), went.tab));
        }
        let focused = layout.shown().focused().pane;
        if focused != went.pane.id {
            let gone = went.pane.id;
            return Err(format!("a layout focuses pane {focused}, not pane {gone} gone to"));
        }
        let kept = match std::mem::take(self) {
            View::Unseated => None,
            View::Going { kept, .. } => kept,
            View::Laid { kept, .. } => Some(kept),
        };
        let kept = match kept {
            Some(mut kept) => {
                kept.screens.retain(|pane, _| layout.holds(*pane));
                if kept.layout.shown().focused().pane != focused {
                    kept.pointer = Pointer::Free;
                }
                kept.layout = layout.clone();
                kept
            },
            None => {
                Kept { layout: layout.clone(), screens: BTreeMap::new(), pointer: Pointer::Free }
            },
        };
        *self = View::Laid { went, kept };
        Ok(())
    }

    fn framed(&mut self, frame: &Frame) -> Result<(), String> {
        self.kept_mut().map_or(Ok(()), |kept| kept.framed(frame))
    }

    fn kept(&self) -> Option<&Kept> {
        match self {
            View::Unseated | View::Going { kept: None, .. } => None,
            View::Going { kept: Some(kept), .. } | View::Laid { kept, .. } => Some(kept),
        }
    }

    fn kept_mut(&mut self) -> Option<&mut Kept> {
        match self {
            View::Unseated | View::Going { kept: None, .. } => None,
            View::Going { kept: Some(kept), .. } | View::Laid { kept, .. } => Some(kept),
        }
    }

    fn selects(&self, mouse: MouseEvent, at: Spot) -> bool {
        let View::Laid { kept, .. } = self else {
            return false;
        };
        let inside = kept
            .focused_screen()
            .zip(on_panes(at))
            .is_some_and(|((inner, _), at)| inner.contains(at));
        inside && selects(kept.tracks_mouse(), mouse)
    }

    fn select_from(&mut self, at: Spot) {
        let View::Laid { kept, .. } = self else {
            return;
        };
        kept.pointer = Pointer::Free;
        let (Some((inner, screen)), Some(at)) = (kept.focused_screen(), on_panes(at)) else {
            return;
        };
        let at = within(spot_in(inner, at), screen.size);
        kept.pointer = Pointer::Selecting(Selection { from: at, to: at });
    }

    fn select_to(&mut self, at: Spot) {
        let Some(kept) = self.kept_mut() else {
            return;
        };
        let Some((inner, screen)) = kept.focused_screen() else {
            return;
        };
        let at = within(spot_in(inner, below_bar(at)), screen.size);
        if let Pointer::Selecting(selection) = &mut kept.pointer {
            selection.to = at;
        }
    }

    fn selected(&mut self) -> Option<String> {
        let kept = self.kept_mut()?;
        let Pointer::Selecting(selection) = kept.pointer else {
            return None;
        };
        let moved = Some(selection).filter(|selection| selection.from != selection.to);
        let screen = kept.focused_screen().map(|(_, screen)| screen);
        let text = moved.zip(screen).map(|(selection, screen)| text(screen, selection));
        let text = text.filter(|text| !text.is_empty());
        kept.pointer = match text {
            Some(_) => Pointer::Shown(selection),
            None => Pointer::Free,
        };
        text
    }

    fn unselect(&mut self) {
        if let Some(kept) = self.kept_mut() {
            kept.pointer = kept.pointer.unselected();
        }
    }

    fn moused(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Moused {
        let View::Laid { went, kept } = self else {
            return Moused::Nothing;
        };
        kept.moused(went.pane.session, mouse, at, width)
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        self.kept()?.draw(buffer, area)
    }

    fn cursor_style(&self) -> (CursorShape, bool) {
        let screen = self.kept().and_then(Kept::focused_screen);
        let Some(cursor) = screen.and_then(|(_, screen)| screen.cursor) else {
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

impl Kept {
    fn framed(&mut self, frame: &Frame) -> Result<(), String> {
        let pane = frame.pane();
        if !self.layout.holds(pane) {
            return Ok(());
        }
        let before = self.screens.remove(&pane);
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
            (Rows::Changed(_), None) => return Ok(()),
            (Rows::Changed(_), Some(screen)) => {
                let sides = |size: Size| format!("{}x{}", size.columns, size.rows);
                let (had, sides) = (sides(screen.size), sides(frame.size()));
                self.screens.insert(pane, screen);
                return Err(format!("a frame of {sides} changes the rows of {had}"));
            },
        };
        if self.layout.shown().focused().pane == pane && size != Some(frame.size()) {
            self.pointer = self.pointer.unselected();
        }
        let screen = Screen {
            size: frame.size(),
            lines,
            cursor: frame.cursor(),
            tracks_mouse: frame.tracks_mouse(),
        };
        self.screens.insert(pane, screen);
        Ok(())
    }

    fn focused_screen(&self) -> Option<(Rect, &Screen)> {
        let focused = self.layout.shown().focused();
        let inner = screen_rect(pane_rect(Rect::default(), focused.rect));
        if inner.is_empty() {
            return None;
        }
        Some((inner, self.screens.get(&focused.pane)?))
    }

    fn tracks_mouse(&self) -> bool {
        self.focused_screen().is_some_and(|(_, screen)| screen.tracks_mouse)
    }

    fn moused(&mut self, session: OwnKey, mouse: MouseEvent, at: Spot, width: u16) -> Moused {
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        if self.pointer == Pointer::Sending {
            if !down {
                return self.sent(mouse, below_bar(at));
            }
            self.pointer = Pointer::Free;
        }
        let layout = &self.layout;
        let Some(spot) = on_panes(at) else {
            let tab = down.then(|| tab_under(layout.tabs(), width, at.column)).flatten();
            return match tab {
                Some(tab) if tab != layout.tab() => Moused::Go(To::Tab { session, tab }),
                Some(_) | None => Moused::Nothing,
            };
        };
        let focused = layout.shown().focused().pane;
        let under = layout
            .shown()
            .panes()
            .iter()
            .find(|placed| pane_rect(Rect::default(), placed.rect).contains(spot));
        if down
            && let Some(under) = under
            && under.pane != focused
        {
            return Moused::Go(To::Pane(OwnPane { session, id: under.pane }));
        }
        self.sent(mouse, spot)
    }

    fn sent(&mut self, mouse: MouseEvent, at: Position) -> Moused {
        let sending = self.pointer == Pointer::Sending;
        let inner = screen_rect(pane_rect(Rect::default(), self.layout.shown().focused().rect));
        if inner.is_empty() {
            if sending {
                self.pointer = Pointer::Free;
            }
            return Moused::Nothing;
        }
        let inside = inner.contains(at);
        let sends = match mouse.kind {
            MouseEventKind::Down(_) => {
                self.pointer = if inside { Pointer::Sending } else { Pointer::Free };
                inside
            },
            MouseEventKind::Drag(_) => sending,
            MouseEventKind::Up(_) => {
                if sending {
                    self.pointer = Pointer::Free;
                }
                sending
            },
            MouseEventKind::Moved
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => inside,
        };
        if !sends {
            return Moused::Nothing;
        }
        mouse_said(mouse, spot_in(inner, at)).map_or(Moused::Nothing, Moused::Sent)
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        let layout = &self.layout;
        let bar = Rect { height: area.height.min(TAB_BAR_ROWS), ..area };
        let panes = Rect { y: area.y + bar.height, height: area.height - bar.height, ..area };
        draw_tabs(buffer, bar, layout);
        let focused = layout.shown().focused().pane;
        let selection = self.pointer.selection();
        let mut cursor = None;
        for placed in layout.shown().panes() {
            let mine = placed.pane == focused;
            let outer = pane_rect(panes, placed.rect);
            let colour = if mine { gruvbox::GREEN } else { gruvbox::GREY };
            border(buffer, panes, outer, &placed.title, Style::new().fg(colour));
            let seen = screen_rect(outer).intersection(panes);
            let Some(screen) = self.screens.get(&placed.pane).filter(|_| !seen.is_empty()) else {
                continue;
            };
            let drawn = draw_screen(buffer, seen, screen, selection.filter(|_| mine));
            if mine {
                cursor = drawn;
            }
        }
        cursor
    }
}

fn on_panes(at: Spot) -> Option<Position> {
    Some(Position::new(at.column, at.row.checked_sub(TAB_BAR_ROWS)?))
}

fn below_bar(at: Spot) -> Position {
    Position::new(at.column, at.row.saturating_sub(TAB_BAR_ROWS))
}

fn pane_rect(panes: Rect, rect: attach::Rect) -> Rect {
    let column = panes.x.checked_add(rect.column).expect("a pane's column is a terminal column");
    let row = panes.y.checked_add(rect.row).expect("a pane's row is a terminal row");
    Rect::new(column, row, rect.columns, rect.rows)
}

fn screen_rect(outer: Rect) -> Rect {
    outer.inner(Margin::new(1, 1))
}

fn spot_in(inner: Rect, at: Position) -> Spot {
    assert!(!inner.is_empty(), "a spot is on a screen of at least one cell");
    let row = at.y.saturating_sub(inner.y).min(inner.height - 1);
    let column = at.x.saturating_sub(inner.x).min(inner.width - 1);
    Spot { row, column }
}

fn tab_under(tabs: &[Tabbed], width: u16, column: u16) -> Option<NonZeroU32> {
    let spans = tab_spans(tabs, width);
    let under = |span: &&TabSpan| (span.column..span.column + span.columns).contains(&column);
    spans.iter().find(under).map(|span| span.id)
}

fn printable(text: &str) -> String {
    text.chars().filter(|character| !character.is_control()).collect()
}

fn tab_spans(tabs: &[Tabbed], width: u16) -> Vec<TabSpan> {
    let mut spans = Vec::new();
    let mut column: u16 = 0;
    for tabbed in tabs {
        let name = printable(&tabbed.name);
        let wanted = u16::try_from(name.width()).unwrap_or(u16::MAX).saturating_add(2);
        let columns = wanted.min(width.saturating_sub(column));
        if columns == 0 {
            break;
        }
        spans.push(TabSpan { id: tabbed.id, column, columns, name });
        column = column.saturating_add(columns).saturating_add(1);
    }
    let end = |span: &TabSpan| u32::from(span.column) + u32::from(span.columns);
    for pair in spans.windows(2) {
        assert!(end(&pair[0]) < u32::from(pair[1].column), "tab spans keep a gap between them");
    }
    if let Some(last) = spans.last() {
        assert!(end(last) <= u32::from(width), "tab spans fit the bar");
    }
    spans
}

fn draw_tabs(buffer: &mut Buffer, bar: Rect, layout: &Layout) {
    if bar.is_empty() {
        return;
    }
    buffer.set_style(bar, Style::new().bg(gruvbox::BG1));
    for span in tab_spans(layout.tabs(), bar.width) {
        let style = if span.id == layout.tab() {
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

fn border(buffer: &mut Buffer, panes: Rect, outer: Rect, title: &str, style: Style) {
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

fn draw_screen(
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
        let size = laid_size(columns, rows);
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

    pub fn mouse(&mut self, mouse: MouseEvent, at: Spot, area: Rect) -> Option<To> {
        match self.view.moused(mouse, at, area.width) {
            Moused::Nothing => None,
            Moused::Go(to) => Some(to),
            Moused::Sent(mouse) => {
                self.attach.send(to_server::Event::Mouse(mouse));
                None
            },
        }
    }

    pub fn selects(&self, mouse: MouseEvent, at: Spot) -> bool {
        self.view.selects(mouse, at)
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
    use lunetad::mux::own::attach::Grapheme;
    use lunetad::mux::own::{OwnClientId, OwnId, OwnMade};

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

    fn session() -> OwnKey {
        OwnKey { id: OwnId::new(1).unwrap(), made: OwnMade::from_nanos(0) }
    }

    fn pane(id: u32) -> OwnPane {
        OwnPane::new(session(), id).unwrap()
    }

    fn went(id: u32, tab: u32) -> Went {
        let client = OwnClientId::new(1).unwrap();
        Went { pane: pane(id), tab: NonZeroU32::new(tab).unwrap(), client }
    }

    fn line(glyphs: Vec<Glyph>) -> Line {
        let columns = glyphs.iter().map(|glyph| glyph.shows.columns()).sum();
        Line::new(glyphs, false, side(columns)).unwrap()
    }

    fn lines(rows: &[&str]) -> Vec<Line> {
        let glyphs = |row: &str| row.chars().map(|it| narrow(&it.to_string())).collect();
        rows.iter().map(|row| line(glyphs(row))).collect()
    }

    fn whole(pane: u32, lines: Vec<Line>, cursor: Option<Cursor>) -> Frame {
        let columns = lines[0].glyphs().iter().map(|glyph| glyph.shows.columns()).sum();
        let columns = side(columns);
        let size = Size { columns, rows: side(u16::try_from(lines.len()).unwrap()) };
        Frame::new(id(pane), size, Rows::Whole(lines), cursor, false).unwrap()
    }

    fn rect(column: u32, row: u32, columns: u32, rows: u32) -> api::Rect {
        api::Rect { column, row, columns, rows }
    }

    fn placed(pane: u32, rect: api::Rect) -> api::Placed {
        api::Placed { pane, rect: Some(rect), title: format!("p{pane}") }
    }

    fn id(id: u32) -> NonZeroU32 {
        NonZeroU32::new(id).unwrap()
    }

    fn laid(tab: u32, columns: u32, rows: u32, shown: api::layout::Shown) -> Layout {
        let tabbed = |id, name: &str| api::Tabbed { id, name: name.to_owned() };
        Layout::heard(api::Layout {
            session: 1,
            tab,
            size: Some(api::Size { columns, rows }),
            shown: Some(shown),
            tabs: vec![tabbed(1, "one"), tabbed(2, "two")],
        })
        .unwrap()
    }

    fn tiled(panes: Vec<api::Placed>, focused: u32) -> api::layout::Shown {
        api::layout::Shown::Tiled(api::Tiled { panes, focused })
    }

    fn two_panes() -> Layout {
        laid(1, 20, 4, tiled(vec![placed(1, rect(0, 0, 10, 4)), placed(2, rect(10, 0, 10, 4))], 1))
    }

    fn screen(view: &View, pane: NonZeroU32) -> Option<&Screen> {
        view.kept()?.screens.get(&pane)
    }

    fn pointer(view: &View) -> Option<Pointer> {
        Some(view.kept()?.pointer)
    }

    fn unlaid(view: &View) -> bool {
        matches!(view, View::Going { kept: None, .. })
    }

    fn heard(view: &mut View, heard: &Heard) {
        view.heard(heard).unwrap();
    }

    fn split_view() -> View {
        let mut view = View::default();
        heard(&mut view, &Heard::Went(went(1, 1)));
        heard(&mut view, &Heard::Layout(two_panes()));
        let cursor =
            Cursor { row: 1, column: 2, shape: attach::CursorShape::Block, blinking: false };
        heard(&mut view, &Heard::Frame(whole(1, lines(&["abcdefgh", "ijklmnop"]), Some(cursor))));
        let wide = glyph(Shows::Wide(grapheme("日")));
        let first = line(
            ["q", "r"]
                .map(narrow)
                .into_iter()
                .chain([wide])
                .chain(["s", "t", "u", "v"].map(narrow))
                .collect(),
        );
        let mut rows = lines(&["yz012345"]);
        rows.insert(0, first);
        heard(&mut view, &Heard::Frame(whole(2, rows, None)));
        view
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE }
    }

    fn spot(row: u16, column: u16) -> Spot {
        Spot { row, column }
    }

    fn row(buffer: &Buffer, y: u16, from: u16, to: u16) -> String {
        (from..to).map(|x| buffer[(x, y)].symbol()).collect()
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
    fn each_pane_draws_in_its_frame_under_the_tab_bar_and_a_larger_area_keeps_a_margin() {
        let view = split_view();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
        buffer[(23, 5)].set_symbol("x");
        let cursor = view.draw(&mut buffer, Rect::new(2, 0, 22, 6));
        assert_eq!(row(&buffer, 0, 2, 14), " one   two  ");
        assert_eq!((buffer[(3, 0)].fg, buffer[(3, 0)].bg), (gruvbox::BG0, gruvbox::GREEN));
        assert_eq!(buffer[(9, 0)].bg, gruvbox::BG2);
        assert_eq!(row(&buffer, 1, 2, 22), "╭ p1 ────╮╭ p2 ────╮");
        assert_eq!(row(&buffer, 2, 2, 12), "│abcdefgh│");
        assert_eq!(row(&buffer, 3, 12, 22), "│yz012345│");
        assert_eq!(row(&buffer, 4, 2, 22), "╰────────╯╰────────╯");
        assert_eq!((buffer[(2, 1)].fg, buffer[(12, 1)].fg), (gruvbox::GREEN, gruvbox::GREY));
        assert_eq!(buffer[(15, 2)].symbol(), "日");
        assert_eq!(row(&buffer, 5, 2, 24), " ".repeat(22));
        assert_eq!(row(&buffer, 1, 22, 24), "  ");
        assert_eq!(cursor, Some(Position::new(5, 3)));
        let mut view = split_view();
        let untitled = api::Placed { title: "\u{7}".to_owned(), ..placed(2, rect(10, 0, 10, 4)) };
        let shown = tiled(vec![placed(1, rect(0, 0, 10, 4)), untitled], 1);
        heard(&mut view, &Heard::Layout(laid(1, 20, 4, shown)));
        view.draw(&mut buffer, Rect::new(2, 0, 22, 6));
        assert_eq!(row(&buffer, 1, 12, 22), "╭────────╮");
    }

    #[test]
    fn a_layout_larger_than_the_area_is_cut_at_its_right_and_bottom() {
        let view = split_view();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 4));
        buffer[(5, 3)].set_symbol("x");
        let cursor = view.draw(&mut buffer, Rect::new(0, 0, 14, 3));
        assert_eq!(row(&buffer, 1, 0, 14), "╭ p1 ────╮╭ p2");
        assert_eq!(row(&buffer, 2, 0, 14), "│abcdefgh││qr ");
        assert_eq!(buffer[(5, 3)].symbol(), "x");
        assert_eq!(cursor, None);
    }

    #[test]
    fn a_press_focuses_the_pane_under_it_or_goes_to_its_tab_and_reaches_the_program_inside() {
        let mut view = split_view();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left));
        let up = mouse(MouseEventKind::Up(MouseButton::Left));
        let sent = |row, column| -> (u32, u32) { (row, column) };
        let at = |moused: Moused| match moused {
            Moused::Sent(said) => Some(sent(said.row, said.column)),
            Moused::Nothing | Moused::Go(_) => None,
        };
        assert_eq!(view.moused(down, spot(2, 14), 22), Moused::Go(To::Pane(pane(2))));
        let tab = NonZeroU32::new(2).unwrap();
        assert_eq!(
            view.moused(down, spot(0, 7), 22),
            Moused::Go(To::Tab { session: session(), tab })
        );
        assert_eq!(view.moused(down, spot(0, 1), 22), Moused::Nothing);
        assert_eq!(view.moused(down, spot(1, 0), 22), Moused::Nothing);
        assert_eq!(view.moused(drag, spot(2, 3), 22), Moused::Nothing);
        assert_eq!(at(view.moused(down, spot(2, 3), 22)), Some((0, 2)));
        assert_eq!(at(view.moused(drag, spot(0, 15), 22)), Some((0, 7)));
        assert_eq!(at(view.moused(up, spot(4, 15), 22)), Some((1, 7)));
        assert_eq!(view.moused(up, spot(2, 3), 22), Moused::Nothing);
    }

    #[test]
    fn a_client_gone_to_another_pane_sends_no_mouse_until_the_layout_that_focuses_it() {
        let mut view = split_view();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        heard(&mut view, &Heard::Went(went(2, 1)));
        assert_eq!(view.moused(down, spot(2, 3), 22), Moused::Nothing);
        assert_eq!(view.moused(down, spot(2, 14), 22), Moused::Nothing);
        assert!(!view.selects(down, spot(2, 3)));
        let panes = vec![placed(1, rect(0, 0, 10, 4)), placed(2, rect(10, 0, 10, 4))];
        heard(&mut view, &Heard::Layout(laid(1, 20, 4, tiled(panes, 2))));
        assert!(matches!(view.moused(down, spot(2, 14), 22), Moused::Sent(_)));
    }

    #[test]
    fn a_selection_ends_a_press_whose_release_never_came() {
        let mut view = split_view();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        assert!(matches!(view.moused(down, spot(2, 3), 22), Moused::Sent(_)));
        view.select_from(spot(2, 1));
        view.select_to(spot(2, 4));
        assert_eq!(view.selected().as_deref(), Some("abcd"));
        assert_eq!(view.moused(down, spot(2, 14), 22), Moused::Go(To::Pane(pane(2))));
    }

    #[test]
    fn a_press_whose_release_never_came_lets_the_next_press_go_to_another_pane_or_tab() {
        let mut view = split_view();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left));
        assert!(matches!(view.moused(down, spot(2, 3), 22), Moused::Sent(_)));
        assert_eq!(view.moused(down, spot(2, 14), 22), Moused::Go(To::Pane(pane(2))));
        assert!(matches!(view.moused(down, spot(2, 3), 22), Moused::Sent(_)));
        let tab = Moused::Go(To::Tab { session: session(), tab: id(2) });
        assert_eq!(view.moused(down, spot(0, 7), 22), tab);
        assert!(matches!(view.moused(down, spot(2, 3), 22), Moused::Sent(_)));
        assert_eq!(view.moused(down, spot(0, 1), 22), Moused::Nothing);
        assert_eq!(view.moused(drag, spot(2, 3), 22), Moused::Nothing);
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
    fn a_drag_selects_in_the_focused_pane_where_its_program_does_not_take_the_mouse() {
        let press = |modifiers| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers,
        };
        assert!(selects(false, press(KeyModifiers::NONE)));
        assert!(!selects(true, press(KeyModifiers::NONE)));
        assert!(selects(true, press(KeyModifiers::SHIFT)));
        let mut view = split_view();
        assert!(view.selects(press(KeyModifiers::NONE), spot(2, 1)));
        assert!(!view.selects(press(KeyModifiers::NONE), spot(1, 1)));
        assert!(!view.selects(press(KeyModifiers::NONE), spot(2, 11)));
        view.select_from(spot(2, 2));
        assert_eq!(view.selected(), None);
        view.select_from(spot(2, 1));
        view.select_to(spot(3, 30));
        assert_eq!(view.selected().as_deref(), Some("abcdefgh\nijklmnop"));
        assert!(matches!(pointer(&view), Some(Pointer::Shown(_))));
        heard(&mut view, &Heard::Frame(whole(2, lines(&["ab"]), None)));
        assert!(matches!(pointer(&view), Some(Pointer::Shown(_))));
        heard(&mut view, &Heard::Frame(whole(1, lines(&["ab"]), None)));
        assert_eq!(pointer(&view), Some(Pointer::Free));
    }

    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size() {
        let mut view = split_view();
        let rows = Rows::Changed(vec![(0, line(vec![narrow("c")]))]);
        let size = Size { columns: side(1), rows: side(1) };
        let frame = Frame::new(NonZeroU32::MIN, size, rows, None, false).unwrap();
        let why = view.heard(&Heard::Frame(frame)).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 8x2");
        assert_eq!(screen(&view, NonZeroU32::MIN).unwrap().lines[0].glyphs()[0], narrow("a"));
    }

    #[test]
    fn a_pane_keeps_its_screen_while_its_tab_lays_it_out_and_waits_for_a_whole_frame_after() {
        let mut view = split_view();
        let (one, two, three) = (id(1), id(2), id(3));
        let pane = Some(placed(1, rect(0, 0, 20, 4)));
        let full = api::layout::Shown::Full(api::Full { pane, hidden: vec![2] });
        heard(&mut view, &Heard::Layout(laid(1, 20, 4, full)));
        assert!(screen(&view, two).is_some(), "a pane hidden by a fullscreen one keeps its screen");
        heard(&mut view, &Heard::Frame(whole(2, lines(&["yz", "01"]), None)));
        assert_eq!(screen(&view, two).unwrap().size, Size { columns: side(2), rows: side(2) });
        heard(&mut view, &Heard::Frame(whole(9, lines(&["no"]), None)));
        assert!(screen(&view, id(9)).is_none(), "a pane its layout does not hold gets no screen");
        heard(&mut view, &Heard::Went(went(2, 1)));
        assert!(screen(&view, one).is_some(), "a client moving inside its tab keeps every screen");
        let alone = tiled(vec![placed(2, rect(0, 0, 20, 4))], 2);
        heard(&mut view, &Heard::Layout(laid(1, 20, 4, alone)));
        assert!(screen(&view, one).is_none(), "a pane its tab no longer lays out has no screen");
        assert!(screen(&view, two).is_some());
        heard(&mut view, &Heard::Went(went(3, 2)));
        assert!(unlaid(&view));
        heard(&mut view, &Heard::Frame(whole(3, lines(&["e"]), None)));
        assert!(unlaid(&view), "a frame before its tab's layout is of the tab left");
        let three_alone = tiled(vec![placed(3, rect(0, 0, 20, 4))], 3);
        heard(&mut view, &Heard::Layout(laid(2, 20, 4, three_alone)));
        let size = Size { columns: side(1), rows: side(1) };
        let changed =
            Frame::new(three, size, Rows::Changed(vec![(0, line(vec![narrow("c")]))]), None, false);
        heard(&mut view, &Heard::Frame(changed.unwrap()));
        assert!(screen(&view, three).is_none(), "changed rows wait for a whole frame of a new tab");
        heard(&mut view, &Heard::Frame(whole(3, lines(&["d"]), None)));
        assert_eq!(screen(&view, three).unwrap().lines[0].glyphs()[0], narrow("d"));
    }

    #[test]
    fn a_layout_follows_the_went_before_it() {
        let mut view = View::default();
        let before = view.heard(&Heard::Layout(two_panes())).unwrap_err();
        assert_eq!(before, "a layout of tab 1 came before a went");
        heard(&mut view, &Heard::Went(went(1, 2)));
        let elsewhere = view.heard(&Heard::Layout(two_panes())).unwrap_err();
        assert_eq!(elsewhere, "a layout of tab 1 came on tab 2");
        heard(&mut view, &Heard::Went(went(2, 1)));
        let astray = view.heard(&Heard::Layout(two_panes())).unwrap_err();
        assert_eq!(astray, "a layout focuses pane 1, not pane 2 gone to");
        assert!(unlaid(&view));
    }
}
