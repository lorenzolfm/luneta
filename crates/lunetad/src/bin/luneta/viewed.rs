use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use lunetad::api::show_request::focus;
use lunetad::folder;
use lunetad::mux::own::api::{self, typed};
use lunetad::mux::own::attach::{self, Frame, Rows};
use lunetad::mux::own::viewer::{self, Greeting, Seen, Viewer};
use lunetad::mux::TerminalKey;
use lunetad::projects::{Size, TerminalId, WorkstreamId};
use lunetad::{Asked, Beside, Found, Heard, Laid, Pane, Rect as Placed, Show, Shown};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::attached::{
    below_bar, border, draw_screen, draw_tab_spans, key_said, mouse_said, on_panes,
    screen_rect, selects, span_under, spot_in, tab_spans, text, within, Pointer, Screen,
    Selection, Spot, TAB_BAR_ROWS,
};
use crate::gruvbox;
use crate::host::Event;

const AGAIN: Duration = Duration::from_secs(1);

const SIDE_MAX: u16 = 1000;

const LOST: &str = "lost: luneta-mux no longer holds it";

const LOOKING: &str = "looking for it in luneta-mux";

const HERE: &str = "luneta runs here";

#[must_use]
pub fn view_size(columns: NonZeroU16, rows: NonZeroU16) -> Size {
    let rows = rows.get().saturating_sub(TAB_BAR_ROWS).max(1);
    let side = |cells: u16| u32::from(cells.min(SIDE_MAX));
    Size::of(side(columns.get()), side(rows)).expect("a side of 1 to 1000 cells is a size")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub workstream: WorkstreamId,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Moused {
    Nothing,
    Go(WorkstreamId),
    Focus(TerminalId),
    Sent(TerminalKey, api::Mouse),
}

pub enum Heeded {
    Quiet,
    Note(String),
    Forgotten(String),
    Lost(String),
}

struct View {
    workstream: WorkstreamId,
    runs_in: Option<TerminalKey>,
    laid: Option<Laid>,
    screens: BTreeMap<TerminalKey, Screen>,
    pointer: Pointer,
    tabs: Vec<Tab>,
    titles: BTreeMap<TerminalId, String>,
}

impl View {
    fn new(workstream: WorkstreamId, runs_in: Option<TerminalKey>) -> View {
        View {
            workstream,
            runs_in,
            laid: None,
            screens: BTreeMap::new(),
            pointer: Pointer::Free,
            tabs: Vec::new(),
            titles: BTreeMap::new(),
        }
    }

    fn laid(&self) -> Option<&Laid> {
        self.laid.as_ref().filter(|laid| laid.workstream() == self.workstream)
    }

    fn focused(&self) -> Option<TerminalId> {
        self.laid()?.focused().map(|pane| pane.terminal)
    }

    fn focused_key(&self) -> Option<TerminalKey> {
        let key = self.laid()?.focused()?.key()?;
        (Some(key) != self.runs_in).then_some(key)
    }

    fn go(&mut self, workstream: WorkstreamId) {
        self.workstream = workstream;
        self.pointer = Pointer::Free;
    }

    fn laid_out(&mut self, laid: Laid) -> bool {
        let before = self.focused();
        let now = laid.focused().map(|pane| pane.terminal);
        let here = laid.workstream() == self.workstream;
        if here && before != now {
            self.pointer = Pointer::Free;
        }
        let empty = here && laid.panes().is_empty();
        self.laid = Some(laid);
        let wanted = self.wanted();
        self.screens.retain(|key, _| wanted.contains(key));
        empty
    }

    fn wanted(&self) -> BTreeSet<TerminalKey> {
        let mut keys = self.laid().map_or_default(Laid::keys);
        if let Some(runs_in) = self.runs_in {
            keys.remove(&runs_in);
        }
        keys
    }

    fn framed(&mut self, key: TerminalKey, frame: &Frame) -> Result<(), String> {
        if !self.wanted().contains(&key) {
            return Ok(());
        }
        let before = self.screens.remove(&key);
        let size = before.as_ref().map(|screen| screen.size);
        let lines = match (frame.rows(), before) {
            (Rows::Whole(lines), _) => lines.clone(),
            (Rows::Changed(rows), Some(screen)) if screen.size == frame.size() => {
                let mut lines = screen.lines;
                let count = usize::from(screen.size.rows.get());
                assert_eq!(lines.len(), count, "a screen holds a line for each of its rows");
                for (index, line) in rows {
                    lines[usize::from(*index)] = line.clone();
                }
                lines
            },
            (Rows::Changed(_), None) => return Ok(()),
            (Rows::Changed(_), Some(screen)) => {
                let sides = |size: attach::Size| format!("{}x{}", size.columns, size.rows);
                let (had, sides) = (sides(screen.size), sides(frame.size()));
                self.screens.insert(key, screen);
                return Err(format!("a frame of {sides} changes the rows of {had}"));
            },
        };
        if self.focused_key() == Some(key) && size != Some(frame.size()) {
            self.pointer = self.pointer.unselected();
        }
        let screen = Screen {
            size: frame.size(),
            lines,
            cursor: frame.cursor(),
            tracks_mouse: frame.tracks_mouse(),
        };
        self.screens.insert(key, screen);
        Ok(())
    }

    fn placed(&self, pane: &Pane) -> Placed {
        match self.laid().and_then(Laid::shown) {
            Some(Shown::Full(full)) if full == pane.terminal => {
                self.laid().map_or(pane.rect, Laid::whole)
            },
            Some(Shown::Full(_) | Shown::Tiled { .. }) | None => pane.rect,
        }
    }

    fn drawn(&self) -> Vec<(&Pane, Placed)> {
        let Some(laid) = self.laid() else {
            return Vec::new();
        };
        let full = match laid.shown() {
            Some(Shown::Full(full)) => Some(full),
            Some(Shown::Tiled { .. }) | None => None,
        };
        let shown = |pane: &&Pane| full.is_none_or(|full| full == pane.terminal);
        let drawn: Vec<(&Pane, Placed)> =
            laid.panes().iter().filter(shown).map(|pane| (pane, self.placed(pane))).collect();
        if full.is_some() {
            assert_eq!(drawn.len(), 1, "a full view draws only the terminal it is full of");
        }
        drawn
    }

    fn focused_screen(&self) -> Option<(Rect, TerminalKey, &Screen)> {
        let pane = self.laid()?.focused()?;
        let inner = screen_rect(pane_rect(Rect::default(), self.placed(pane)));
        if inner.is_empty() {
            return None;
        }
        let key = self.focused_key()?;
        Some((inner, key, self.screens.get(&key)?))
    }

    fn named_tabs(&self) -> impl Iterator<Item = (WorkstreamId, &str)> {
        self.tabs.iter().map(|tab| (tab.workstream, tab.name.as_str()))
    }

    fn moused(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Moused {
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        if self.pointer == Pointer::Sending {
            if !down {
                return self.sent(mouse, below_bar(at));
            }
            self.pointer = Pointer::Free;
        }
        let Some(spot) = on_panes(at) else {
            let spans = tab_spans(self.named_tabs(), width);
            let tab = down.then(|| span_under(&spans, at.column)).flatten();
            return match tab {
                Some(tab) if tab != self.workstream => Moused::Go(tab),
                Some(_) | None => Moused::Nothing,
            };
        };
        let under = self
            .drawn()
            .into_iter()
            .find(|(_, placed)| pane_rect(Rect::default(), *placed).contains(spot))
            .map(|(pane, _)| pane.terminal);
        if down
            && let Some(under) = under
            && Some(under) != self.focused()
        {
            return Moused::Focus(under);
        }
        self.sent(mouse, spot)
    }

    fn sent(&mut self, mouse: MouseEvent, at: Position) -> Moused {
        let sending = self.pointer == Pointer::Sending;
        let Some((inner, key, _)) = self.focused_screen() else {
            if sending {
                self.pointer = Pointer::Free;
            }
            return Moused::Nothing;
        };
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
        let said = sends.then(|| mouse_said(mouse, spot_in(inner, at))).flatten();
        said.map_or(Moused::Nothing, |said| Moused::Sent(key, said))
    }

    fn selects(&self, mouse: MouseEvent, at: Spot) -> bool {
        let Some((inner, _, screen)) = self.focused_screen() else {
            return false;
        };
        let inside = on_panes(at).is_some_and(|at| inner.contains(at));
        inside && selects(screen.tracks_mouse, mouse)
    }

    fn select_from(&mut self, at: Spot) {
        self.pointer = Pointer::Free;
        let (Some((inner, _, screen)), Some(at)) = (self.focused_screen(), on_panes(at)) else {
            return;
        };
        let at = within(spot_in(inner, at), screen.size);
        self.pointer = Pointer::Selecting(Selection { from: at, to: at });
    }

    fn select_to(&mut self, at: Spot) {
        let Some((inner, _, screen)) = self.focused_screen() else {
            return;
        };
        let at = within(spot_in(inner, below_bar(at)), screen.size);
        if let Pointer::Selecting(selection) = &mut self.pointer {
            selection.to = at;
        }
    }

    fn selected(&mut self) -> Option<String> {
        let Pointer::Selecting(selection) = self.pointer else {
            return None;
        };
        let moved = Some(selection).filter(|selection| selection.from != selection.to);
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let text = moved.zip(screen).map(|(selection, screen)| text(screen, selection));
        let text = text.filter(|text| !text.is_empty());
        self.pointer = match text {
            Some(_) => Pointer::Shown(selection),
            None => Pointer::Free,
        };
        text
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let bar = Rect { height: area.height.min(TAB_BAR_ROWS), ..area };
        let panes = Rect { y: area.y + bar.height, height: area.height - bar.height, ..area };
        draw_tab_spans(buffer, bar, &tab_spans(self.named_tabs(), bar.width), self.workstream);
        let focused = self.focused();
        let selection = self.pointer.selection();
        let mut cursor = None;
        for (pane, placed) in self.drawn() {
            let mine = Some(pane.terminal) == focused;
            let outer = pane_rect(panes, placed);
            let colour = if mine { gruvbox::GREEN } else { gruvbox::GREY };
            let title = self.titles.get(&pane.terminal).map_or("", String::as_str);
            border(buffer, panes, outer, title, Style::new().fg(colour));
            let seen = screen_rect(outer).intersection(panes);
            if seen.is_empty() {
                continue;
            }
            let said = match pane.found {
                Found::Key(key) if Some(key) == self.runs_in => HERE,
                Found::Key(key) => {
                    if let Some(screen) = self.screens.get(&key) {
                        let drawn = draw_screen(buffer, seen, screen, selection.filter(|_| mine));
                        if mine {
                            cursor = drawn;
                        }
                    }
                    continue;
                },
                Found::Lost => LOST,
                Found::Looking => LOOKING,
            };
            let style = Style::new().fg(gruvbox::GREY);
            buffer.set_stringn(seen.x, seen.y, said, usize::from(seen.width), style);
        }
        cursor
    }

    fn cursor_style(&self) -> (CursorShape, bool) {
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let Some(cursor) = screen.and_then(|screen| screen.cursor) else {
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

fn pane_rect(panes: Rect, rect: Placed) -> Rect {
    let column = panes.x.checked_add(rect.column).expect("a pane's column is a terminal column");
    let row = panes.y.checked_add(rect.row).expect("a pane's row is a terminal row");
    Rect::new(column, row, rect.columns, rect.rows)
}

struct Watcher {
    number: u64,
    viewer: Viewer,
    showing: BTreeSet<TerminalKey>,
}

pub struct Viewed {
    number: u64,
    folder: PathBuf,
    show: Option<Show>,
    shown_at: Instant,
    size: Size,
    greeting: Greeting,
    view: View,
    watcher: Option<Watcher>,
    watchers: u64,
    watched_at: Option<Instant>,
    opened: BTreeSet<WorkstreamId>,
    events: Sender<Event>,
}

impl Viewed {
    pub fn start(
        number: u64,
        folder: PathBuf,
        workstream: WorkstreamId,
        size: Size,
        greeting: Greeting,
        events: &Sender<Event>,
    ) -> Viewed {
        let mut viewed = Viewed {
            number,
            folder,
            show: None,
            shown_at: Instant::now(),
            size,
            greeting,
            view: View::new(workstream, greeting.runs_in),
            watcher: None,
            watchers: 0,
            watched_at: None,
            opened: BTreeSet::new(),
            events: events.clone(),
        };
        viewed.shown();
        viewed
    }

    fn shown(&mut self) {
        self.shown_at = Instant::now();
        let (number, events) = (self.number, self.events.clone());
        let lost = |why: String| drop(events.send(Event::Shown(number, Heard::Lost(why))));
        let opened = match folder::open(&self.folder) {
            Ok(Some(opened)) => opened,
            Ok(None) => return lost("lunetad has made no folder yet".to_owned()),
            Err(refusal) => return lost(refusal.to_string()),
        };
        let events = self.events.clone();
        let told = move |heard| drop(events.send(Event::Shown(number, heard)));
        let workstream = self.view.workstream;
        self.show = Some(lunetad::show(opened, workstream, self.size, told));
    }

    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    #[must_use]
    pub fn workstream(&self) -> WorkstreamId {
        self.view.workstream
    }

    #[must_use]
    pub fn focused(&self) -> Option<TerminalId> {
        self.view.focused()
    }

    pub fn heard(&mut self, heard: Heard) -> Heeded {
        match heard {
            Heard::Laid(laid) => {
                if self.view.laid_out(laid) && self.opened.insert(self.view.workstream) {
                    self.ask(Asked::Open { beside: Beside::Largest, argv: Vec::new() });
                }
                self.watch();
                Heeded::Quiet
            },
            Heard::Refused(why) => Heeded::Note(format!("lunetad refused: {why}")),
            Heard::Forgotten(why) => {
                self.show = None;
                Heeded::Forgotten(why)
            },
            Heard::Lost(why) => {
                self.show = None;
                Heeded::Lost(why)
            },
        }
    }

    fn watch(&mut self) {
        let wanted = self.view.wanted();
        match &mut self.watcher {
            Some(watcher) if watcher.showing == wanted => {},
            Some(watcher) => {
                watcher.viewer.show(wanted.clone());
                watcher.showing = wanted;
            },
            None if wanted.is_empty() => {},
            None if self.watched_at.is_some_and(|at| at.elapsed() < AGAIN) => {},
            None => {
                self.watchers += 1;
                self.watched_at = Some(Instant::now());
                let (number, watcher, events) = (self.number, self.watchers, self.events.clone());
                let socket = match lunetad::mux::own::server::socket() {
                    Ok(socket) => socket,
                    Err(failure) => {
                        let seen = Seen::Lost(failure.to_string());
                        let _ = events.send(Event::Seen { number, watcher, seen });
                        return;
                    },
                };
                let told = move |seen| drop(events.send(Event::Seen { number, watcher, seen }));
                let viewer = viewer::view(socket, self.greeting, told);
                viewer.show(wanted.clone());
                self.watcher = Some(Watcher { number: watcher, viewer, showing: wanted });
            },
        }
    }

    pub fn seen(&mut self, watcher: u64, seen: Seen) -> Heeded {
        let Some(watching) = self.watcher.as_ref().filter(|it| it.number == watcher) else {
            return Heeded::Quiet;
        };
        let framed = match seen {
            Seen::Frame(framed) => framed,
            Seen::Refused(why) => return Heeded::Note(format!("luneta-mux refused: {why}")),
            Seen::Lost(why) => {
                self.unwatch();
                return Heeded::Note(format!("luneta's view of luneta-mux: {why}"));
            },
        };
        let frame = framed.frame();
        let mut keys = watching.showing.iter().filter(|key| key.id == frame.pane());
        let Some(key) = keys.next().copied() else {
            return Heeded::Quiet;
        };
        assert_eq!(keys.next(), None, "a view's keys name each pane of luneta-mux's once");
        match self.view.framed(key, frame) {
            Ok(()) => Heeded::Quiet,
            Err(why) => {
                self.unwatch();
                Heeded::Note(format!("luneta-mux said what luneta cannot follow: {why}"))
            },
        }
    }

    fn unwatch(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.viewer.give_up("luneta lets go of its view".to_owned());
        }
        self.view.screens.clear();
    }

    pub fn ticked(&mut self) {
        if self.show.is_none() && self.shown_at.elapsed() >= AGAIN {
            self.shown();
        }
        if self.watcher.is_none() {
            self.watch();
        }
    }

    pub fn ask(&self, asked: Asked) {
        if let Some(show) = &self.show {
            show.ask(asked);
        }
    }

    pub fn go(&mut self, workstream: WorkstreamId) {
        if workstream == self.view.workstream {
            return;
        }
        self.view.go(workstream);
        self.ask(Asked::Go(workstream));
        self.watch();
    }

    pub fn open_in(&mut self, workstream: WorkstreamId, argv: Vec<String>) {
        if workstream != self.view.workstream {
            return;
        }
        self.opened.insert(workstream);
        self.ask(Asked::Open { beside: Beside::Largest, argv });
    }

    pub fn open_beside_focused(&mut self, argv: Vec<String>) {
        self.opened.insert(self.view.workstream);
        self.ask(Asked::Open { beside: Beside::Focused, argv });
    }

    pub fn tabs(&mut self, tabs: Vec<Tab>, titles: BTreeMap<TerminalId, String>) {
        self.view.tabs = tabs;
        self.view.titles = titles;
    }

    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) {
        let size = view_size(columns, rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.ask(Asked::Size(size));
    }

    fn type_in(&self, key: TerminalKey, event: typed::Event) {
        if let Some(watcher) = &self.watcher
            && watcher.showing.contains(&key)
        {
            watcher.viewer.typed(key, event);
        }
    }

    fn type_focused(&self, event: typed::Event) {
        if let Some(key) = self.view.focused_key() {
            self.type_in(key, event);
        }
    }

    pub fn key(&mut self, key: &KeyEvent) {
        self.unselect();
        if let Some(key) = key_said(key) {
            self.type_focused(typed::Event::Key(key));
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.unselect();
        self.type_focused(typed::Event::Paste(api::Paste { text: text.to_owned() }));
    }

    pub fn focus(&self, focused: bool) {
        self.type_focused(typed::Event::Focus(api::Focus { focused }));
    }

    pub fn mouse(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Option<WorkstreamId> {
        match self.view.moused(mouse, at, width) {
            Moused::Nothing => None,
            Moused::Go(workstream) => Some(workstream),
            Moused::Focus(terminal) => {
                self.ask(Asked::Focus(focus::To::Terminal(terminal.get())));
                None
            },
            Moused::Sent(key, said) => {
                self.type_in(key, typed::Event::Mouse(said));
                None
            },
        }
    }

    #[must_use]
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
        self.view.pointer = self.view.pointer.unselected();
    }

    #[must_use]
    pub fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        self.view.draw(buffer, area)
    }

    #[must_use]
    pub fn cursor_style(&self) -> (CursorShape, bool) {
        self.view.cursor_style()
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use crossterm::event::{KeyModifiers, MouseButton};
    use lunetad::api::{show_request, show_response};
    use lunetad::mux::own::attach::{Cursor, Glyph, Grapheme, Line, Shows};
    use lunetad::mux::own::OwnMade;

    use super::*;

    fn side(count: u16) -> NonZeroU16 {
        NonZeroU16::new(count).unwrap()
    }

    fn narrow(it: &str) -> Glyph {
        let shows = Shows::Narrow(Grapheme::new(it.to_owned()).unwrap());
        Glyph { shows, style: attach::Style::default() }
    }

    fn lines(rows: &[&str]) -> Vec<Line> {
        let line = |row: &str| {
            let glyphs: Vec<Glyph> = row.chars().map(|it| narrow(&it.to_string())).collect();
            Line::new(glyphs, false, side(u16::try_from(row.len()).unwrap())).unwrap()
        };
        rows.iter().map(|row| line(row)).collect()
    }

    fn key(id: u32) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(0) }
    }

    fn whole(id: u32, rows: &[&str], cursor: Option<Cursor>) -> Frame {
        let lines = lines(rows);
        let columns = side(u16::try_from(rows[0].len()).unwrap());
        let size = attach::Size { columns, rows: side(u16::try_from(lines.len()).unwrap()) };
        Frame::new(NonZeroU32::new(id).unwrap(), size, Rows::Whole(lines), cursor, false).unwrap()
    }

    fn workstream(id: u64) -> WorkstreamId {
        WorkstreamId::of(id).unwrap()
    }

    fn terminal(id: u64) -> TerminalId {
        TerminalId::of(id).unwrap()
    }

    fn pane(terminal: u64, rect: (u32, u32, u32, u32), found: Option<u32>) -> show_response::Pane {
        use show_response::pane::Found as Said;
        let (column, row, columns, rows) = rect;
        let found = match found {
            Some(id) => Said::Key(api_key(id)),
            None => Said::Lost(()),
        };
        show_response::Pane {
            terminal,
            rect: Some(show_response::Rect { column, row, columns, rows }),
            found: Some(found),
        }
    }

    fn api_key(id: u32) -> lunetad::api::TerminalKey {
        lunetad::api::TerminalKey::from(key(id))
    }

    fn laid(panes: Vec<show_response::Pane>, focused: u64, full: Option<u64>) -> Laid {
        Laid::heard(show_response::Laid {
            workstream: 1,
            size: Some(show_request::Size { columns: 20, rows: 4 }),
            panes,
            focused: Some(focused),
            full,
        })
        .unwrap()
    }

    fn halves() -> Vec<show_response::Pane> {
        vec![pane(11, (0, 0, 10, 4), Some(1)), pane(12, (10, 0, 10, 4), Some(2))]
    }

    fn split_view() -> View {
        let mut view = View::new(workstream(1), None);
        view.tabs = vec![
            Tab { workstream: workstream(1), name: "one".to_owned() },
            Tab { workstream: workstream(2), name: "two".to_owned() },
        ];
        view.titles = [(terminal(11), "p1".to_owned()), (terminal(12), "p2".to_owned())].into();
        assert!(!view.laid_out(laid(halves(), 11, None)));
        let cursor =
            Cursor { row: 1, column: 2, shape: attach::CursorShape::Block, blinking: false };
        view.framed(key(1), &whole(1, &["abcdefgh", "ijklmnop"], Some(cursor))).unwrap();
        view.framed(key(2), &whole(2, &["qrstuvwx", "yz012345"], None)).unwrap();
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
    fn each_terminal_draws_in_its_frame_under_the_workstreams_and_a_larger_area_keeps_a_margin() {
        let view = split_view();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
        let cursor = view.draw(&mut buffer, Rect::new(2, 0, 22, 6));
        assert_eq!(row(&buffer, 0, 2, 14), " one   two  ");
        assert_eq!((buffer[(3, 0)].fg, buffer[(3, 0)].bg), (gruvbox::BG0, gruvbox::GREEN));
        assert_eq!(row(&buffer, 1, 2, 22), "╭ p1 ────╮╭ p2 ────╮");
        assert_eq!(row(&buffer, 2, 2, 12), "│abcdefgh│");
        assert_eq!(row(&buffer, 3, 12, 22), "│yz012345│");
        assert_eq!(row(&buffer, 4, 2, 22), "╰────────╯╰────────╯");
        assert_eq!((buffer[(2, 1)].fg, buffer[(12, 1)].fg), (gruvbox::GREEN, gruvbox::GREY));
        assert_eq!(row(&buffer, 5, 2, 24), " ".repeat(22));
        assert_eq!(cursor, Some(Position::new(5, 3)));
        let mut small = Buffer::empty(Rect::new(0, 0, 14, 3));
        assert_eq!(view.draw(&mut small, Rect::new(0, 0, 14, 3)), None);
        assert_eq!(row(&small, 1, 0, 14), "╭ p1 ────╮╭ p2");
        assert_eq!(row(&small, 2, 0, 14), "│abcdefgh││qrs");
    }

    #[test]
    fn a_full_terminal_fills_the_view_and_a_lost_one_or_the_one_luneta_runs_in_says_so() {
        let mut view = split_view();
        view.laid_out(laid(halves(), 12, Some(12)));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        view.draw(&mut buffer, Rect::new(0, 0, 20, 5));
        assert_eq!(row(&buffer, 1, 0, 20), "╭ p2 ──────────────╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        let mut here = View::new(workstream(1), Some(key(2)));
        let lost = vec![pane(11, (0, 0, 10, 4), None), pane(12, (10, 0, 10, 4), Some(2))];
        here.laid_out(laid(lost, 11, None));
        assert_eq!(here.wanted(), BTreeSet::new(), "luneta never shows the terminal it runs in");
        here.draw(&mut buffer, Rect::new(0, 0, 20, 5));
        assert_eq!(row(&buffer, 2, 1, 9), "lost: lu");
        assert_eq!(row(&buffer, 2, 11, 19), "luneta r");
    }

    #[test]
    fn a_press_focuses_the_terminal_under_it_or_goes_to_its_workstream_or_reaches_the_program() {
        let mut view = split_view();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left));
        let up = mouse(MouseEventKind::Up(MouseButton::Left));
        let at = |moused: Moused| match moused {
            Moused::Sent(key, said) => Some((key.id.get(), said.row, said.column)),
            Moused::Nothing | Moused::Go(_) | Moused::Focus(_) => None,
        };
        assert_eq!(view.moused(down, spot(2, 14), 22), Moused::Focus(terminal(12)));
        assert_eq!(view.moused(down, spot(0, 7), 22), Moused::Go(workstream(2)));
        assert_eq!(view.moused(down, spot(0, 1), 22), Moused::Nothing);
        assert_eq!(view.moused(down, spot(1, 0), 22), Moused::Nothing);
        assert_eq!(at(view.moused(down, spot(2, 3), 22)), Some((1, 0, 2)));
        assert_eq!(at(view.moused(drag, spot(0, 15), 22)), Some((1, 0, 7)));
        assert_eq!(at(view.moused(up, spot(4, 15), 22)), Some((1, 1, 7)));
        assert_eq!(view.moused(up, spot(2, 3), 22), Moused::Nothing);
    }

    #[test]
    fn a_drag_selects_in_the_focused_terminal_and_a_new_size_lets_it_go() {
        let mut view = split_view();
        let press = mouse(MouseEventKind::Down(MouseButton::Left));
        assert!(view.selects(press, spot(2, 1)));
        assert!(!view.selects(press, spot(2, 11)));
        view.select_from(spot(2, 1));
        view.select_to(spot(3, 30));
        assert_eq!(view.selected().as_deref(), Some("abcdefgh\nijklmnop"));
        assert!(matches!(view.pointer, Pointer::Shown(_)));
        view.framed(key(2), &whole(2, &["ab"], None)).unwrap();
        assert!(matches!(view.pointer, Pointer::Shown(_)));
        view.framed(key(1), &whole(1, &["ab"], None)).unwrap();
        assert_eq!(view.pointer, Pointer::Free);
    }

    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size_and_a_terminal_laid_out_no_more_drops() {
        let mut view = split_view();
        let changed = Rows::Changed(vec![(0, lines(&["c"]).remove(0))]);
        let size = attach::Size { columns: side(1), rows: side(1) };
        let frame = Frame::new(NonZeroU32::MIN, size, changed, None, false).unwrap();
        let why = view.framed(key(1), &frame).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 8x2");
        view.laid_out(laid(vec![pane(12, (0, 0, 20, 4), Some(2))], 12, None));
        assert_eq!(view.screens.keys().copied().collect::<Vec<_>>(), [key(2)]);
        view.framed(key(1), &whole(1, &["no"], None)).unwrap();
        assert!(!view.screens.contains_key(&key(1)), "a terminal its view lacks gets no screen");
        view.go(workstream(2));
        assert_eq!(view.focused(), None, "a view of another workstream lays out nothing here");
    }
}
