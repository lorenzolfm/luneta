use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use lunetad::folder;
use lunetad::mux::own::api::{self, typed};
use lunetad::mux::own::frames::{self, Frame, Rows};
use lunetad::mux::own::viewer::{self, Greeting, Reach, Seen, Viewer};
use lunetad::mux::TerminalKey;
use lunetad::projects::{Size, ViewId, WorktreeId};
use lunetad::{
    Asked, Beside, Direction, Found, Heard, Laid, View, Rect as Placed, Show, Where,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::attached::{
    below_bar, border, draw_screen, draw_tab_spans, key_said, link, mouse_said, on_views,
    screen_rect, selects, span_under, spot_in, tab_spans, text, within, Pointer, Screen,
    Selection, Spot, TAB_BAR_ROWS,
};
use crate::gruvbox;
use crate::host::Event;
use crate::machines::Machine;

const AGAIN: Duration = Duration::from_secs(1);

const SIDE_MAX: u16 = 1000;

const LOST: &str = "lost: luneta-mux no longer holds it";

const LOOKING: &str = "looking for it in luneta-mux";

const HERE: &str = "luneta runs here";

const WELCOME: [&str; 4] =
    ["Welcome", "", "This view runs no terminal.", "d d  close it   Ctrl hjkl  another view"];

const CLOSE: char = 'd';

#[must_use]
pub fn layout_size(columns: NonZeroU16, rows: NonZeroU16) -> Size {
    let rows = rows.get().saturating_sub(TAB_BAR_ROWS).max(1);
    let side = |cells: u16| u32::from(cells.min(SIDE_MAX));
    Size::of(side(columns.get()), side(rows)).expect("a side of 1 to 1000 cells is a size")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub worktree: WorktreeId,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Moused {
    Nothing,
    Go(WorktreeId),
    Focus(ViewId),
    Sent(TerminalKey, api::Mouse),
}

pub enum Heeded {
    Quiet,
    Followed,
    Edged(Direction),
    Sent(WorktreeId),
    Note(String),
    Emptied,
    Forgotten(String),
    Lost(String),
}

struct Layout {
    worktree: WorktreeId,
    runs_in: Option<TerminalKey>,
    laid: Option<Laid>,
    screens: BTreeMap<TerminalKey, Screen>,
    pointer: Pointer,
    tabs: Vec<Tab>,
    titles: BTreeMap<ViewId, String>,
    closing: Closing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    Unarmed,
    Armed { view: ViewId, input: u64 },
    Closed,
}

impl Layout {
    fn new(worktree: WorktreeId, runs_in: Option<TerminalKey>) -> Layout {
        Layout {
            worktree,
            runs_in,
            laid: None,
            screens: BTreeMap::new(),
            pointer: Pointer::Free,
            tabs: Vec::new(),
            titles: BTreeMap::new(),
            closing: Closing::Unarmed,
        }
    }

    fn laid(&self) -> Option<&Laid> {
        self.laid.as_ref()
    }

    fn focused(&self) -> Option<ViewId> {
        self.laid()?.focused().map(|view| view.id)
    }

    fn welcomes(&self) -> bool {
        self.laid().and_then(Laid::focused).is_some_and(|view| view.found == Found::Welcome)
    }

    fn closes(&mut self, key: &KeyEvent, input: u64) -> Option<ViewId> {
        assert!(self.welcomes(), "only a welcome view is closed by its keys");
        let focused = self.focused().expect("a welcome view welcomes when focused");
        let closing = std::mem::replace(&mut self.closing, Closing::Unarmed);
        let pressed = key.kind == KeyEventKind::Press && key.modifiers.is_empty();
        if !pressed || key.code != KeyCode::Char(CLOSE) {
            return None;
        }
        let next = |armed: u64| armed.checked_add(1) == Some(input);
        let twice = matches!(closing, Closing::Armed { view, input } if view == focused && next(input));
        if twice {
            self.closing = Closing::Closed;
            return Some(focused);
        }
        self.closing = Closing::Armed { view: focused, input };
        None
    }

    fn held_after_close(&mut self, key: &KeyEvent) -> bool {
        if self.closing != Closing::Closed {
            return false;
        }
        let held = key.kind == KeyEventKind::Repeat && key.code == KeyCode::Char(CLOSE);
        if !held {
            self.closing = Closing::Unarmed;
        }
        held
    }

    fn focused_key(&self) -> Option<TerminalKey> {
        let key = self.laid()?.focused()?.key()?;
        (Some(key) != self.runs_in).then_some(key)
    }

    fn go(&mut self, worktree: WorktreeId) {
        self.worktree = worktree;
        self.laid = None;
        self.screens.clear();
        self.pointer = Pointer::Free;
    }

    fn left(&mut self, worktree: WorktreeId) -> bool {
        if worktree != self.worktree {
            return false;
        }
        self.go(worktree);
        assert!(self.wanted().is_empty(), "a worktree gone shows no terminal");
        true
    }

    fn followed(&mut self, laid: &Laid) -> bool {
        let arrived = self.laid.is_some();
        let follows = arrived && laid.worktree() != self.worktree;
        if follows {
            self.worktree = laid.worktree();
            self.pointer = Pointer::Free;
        }
        follows
    }

    fn laid_out(&mut self, laid: Laid) -> bool {
        if laid.worktree() != self.worktree {
            return false;
        }
        if self.focused() != laid.focused().map(|view| view.id) {
            self.pointer = Pointer::Free;
            if matches!(self.closing, Closing::Armed { .. }) {
                self.closing = Closing::Unarmed;
            }
        }
        let empty = laid.is_empty();
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

    fn framed(&mut self, frame: &Frame) -> Result<(), String> {
        let key = frame.terminal();
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
                let sides = |size: frames::Size| format!("{}x{}", size.columns, size.rows);
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

    fn drawn(&self) -> Vec<(&View, Placed)> {
        self.laid().map_or_default(Laid::drawn)
    }

    fn focused_screen(&self) -> Option<(Rect, TerminalKey, &Screen)> {
        let (_, placed) = self.laid()?.focused_at()?;
        let inner = screen_rect(view_rect(Rect::default(), placed));
        if inner.is_empty() {
            return None;
        }
        let key = self.focused_key()?;
        Some((inner, key, self.screens.get(&key)?))
    }

    fn edged(&self, worktree: WorktreeId, direction: Direction) -> Option<Direction> {
        (worktree == self.worktree).then_some(direction)
    }

    fn named_tabs(&self) -> impl Iterator<Item = (WorktreeId, &str)> {
        self.tabs.iter().map(|tab| (tab.worktree, tab.name.as_str()))
    }

    fn moused(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Moused {
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        if self.pointer == Pointer::Sending {
            if !down {
                return self.sent(mouse, below_bar(at));
            }
            self.pointer = Pointer::Free;
        }
        let Some(spot) = on_views(at) else {
            let spans = tab_spans(self.named_tabs(), width);
            let tab = down.then(|| span_under(&spans, at.column)).flatten();
            return match tab {
                Some(tab) if tab != self.worktree => Moused::Go(tab),
                Some(_) | None => Moused::Nothing,
            };
        };
        let under = self
            .drawn()
            .into_iter()
            .find(|(_, placed)| view_rect(Rect::default(), *placed).contains(spot))
            .map(|(view, _)| view.id);
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
        let inside = on_views(at).is_some_and(|at| inner.contains(at));
        inside && selects(screen.tracks_mouse, mouse)
    }

    fn select_from(&mut self, at: Spot) {
        self.pointer = Pointer::Free;
        let (Some((inner, _, screen)), Some(at)) = (self.focused_screen(), on_views(at)) else {
            return;
        };
        let at = spot_in(inner, at);
        let held = within(at, screen.size);
        self.pointer = if held == at {
            Pointer::Pressed(at)
        } else {
            Pointer::Selecting(Selection { from: held, to: held })
        };
    }

    fn select_to(&mut self, at: Spot) {
        let Some((inner, _, screen)) = self.focused_screen() else {
            return;
        };
        let at = within(spot_in(inner, below_bar(at)), screen.size);
        self.pointer = match self.pointer {
            Pointer::Pressed(from) if from != at => Pointer::Selecting(Selection { from, to: at }),
            Pointer::Selecting(selection) => Pointer::Selecting(Selection { to: at, ..selection }),
            Pointer::Free | Pointer::Sending | Pointer::Pressed(_) | Pointer::Shown(_) => {
                return;
            },
        };
    }

    fn selected(&mut self) -> Option<String> {
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let chosen = match self.pointer {
            Pointer::Pressed(at) => screen.and_then(|screen| link(screen, at)),
            Pointer::Selecting(selection) if selection.from != selection.to => screen
                .map(|screen| (selection, text(screen, selection)))
                .filter(|(_, text)| !text.is_empty()),
            Pointer::Selecting(_) => None,
            Pointer::Free | Pointer::Sending | Pointer::Shown(_) => return None,
        };
        self.pointer = chosen.as_ref().map_or(Pointer::Free, |(lit, _)| Pointer::Shown(*lit));
        chosen.map(|(_, text)| text)
    }

    fn pointed(&self, at: Spot) -> Option<Selection> {
        let (inner, _, screen) = self.focused_screen()?;
        let at = on_views(at).filter(|at| inner.contains(*at))?;
        if screen.tracks_mouse {
            return None;
        }
        let at = spot_in(inner, at);
        if within(at, screen.size) != at {
            return None;
        }
        link(screen, at).map(|(span, _)| span)
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect, hover: Option<Spot>) -> Option<Position> {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let bar = Rect { height: area.height.min(TAB_BAR_ROWS), ..area };
        let views = Rect { y: area.y + bar.height, height: area.height - bar.height, ..area };
        draw_tab_spans(buffer, bar, &tab_spans(self.named_tabs(), bar.width), self.worktree);
        let focused = self.focused();
        let selection = self.pointer.selection();
        let pointed = hover.and_then(|at| self.pointed(at));
        let mut cursor = None;
        for (view, placed) in self.drawn() {
            let mine = Some(view.id) == focused;
            let outer = view_rect(views, placed);
            let colour = if mine { gruvbox::GREEN } else { gruvbox::GREY };
            let title = self.titles.get(&view.id).map_or("", String::as_str);
            border(buffer, views, outer, title, Style::new().fg(colour));
            let seen = screen_rect(outer).intersection(views);
            if seen.is_empty() {
                continue;
            }
            let said = match view.found {
                Found::Key(key) if Some(key) == self.runs_in => HERE,
                Found::Key(key) => {
                    if let Some(screen) = self.screens.get(&key) {
                        let lit = |span: Option<Selection>| span.filter(|_| mine);
                        let drawn = draw_screen(buffer, seen, screen, lit(selection), lit(pointed));
                        if mine {
                            cursor = drawn;
                        }
                    }
                    continue;
                },
                Found::Lost => LOST,
                Found::Looking => LOOKING,
                Found::Welcome => {
                    let style = Style::new().fg(if mine { gruvbox::GREEN } else { gruvbox::GREY });
                    for (line, y) in WELCOME.iter().zip(seen.top()..seen.bottom()) {
                        buffer.set_stringn(seen.x, y, line, usize::from(seen.width), style);
                    }
                    continue;
                },
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
            frames::CursorShape::Block => CursorShape::Block,
            frames::CursorShape::Bar => CursorShape::Beam,
            frames::CursorShape::Underline => CursorShape::Underline,
            frames::CursorShape::BlockHollow => CursorShape::HollowBlock,
        };
        (shape, cursor.blinking)
    }
}

fn view_rect(views: Rect, rect: Placed) -> Rect {
    let column = views.x.checked_add(rect.column).expect("a view's column is a terminal column");
    let row = views.y.checked_add(rect.row).expect("a view's row is a terminal row");
    Rect::new(column, row, rect.columns, rect.rows)
}

struct Watcher {
    number: u64,
    viewer: Viewer,
    showing: BTreeSet<TerminalKey>,
}

fn where_on(machine: &Machine, runs_in_here: Option<TerminalKey>) -> Where {
    match machine {
        Machine::Local => Where::Here { runs_in: runs_in_here },
        Machine::Remote(destination) => Where::On(destination.clone()),
    }
}

pub struct Viewed {
    number: u64,
    folder: PathBuf,
    machine: Machine,
    show: Option<Show>,
    shown_at: Instant,
    laid_in: Option<WorktreeId>,
    size: Size,
    greeting: Greeting,
    runs_in_here: Option<TerminalKey>,
    layout: Layout,
    watcher: Option<Watcher>,
    watchers: u64,
    watched_at: Option<Instant>,
    opened: BTreeSet<WorktreeId>,
    events: Sender<Event>,
}

impl Viewed {
    pub fn start(
        number: u64,
        folder: PathBuf,
        machine: Machine,
        worktree: WorktreeId,
        size: Size,
        greeting: Greeting,
        runs_in_here: Option<TerminalKey>,
        events: &Sender<Event>,
    ) -> Viewed {
        let runs_in = where_on(&machine, runs_in_here).runs_in();
        let mut viewed = Viewed {
            number,
            folder,
            machine,
            show: None,
            shown_at: Instant::now(),
            laid_in: None,
            size,
            greeting,
            runs_in_here,
            layout: Layout::new(worktree, runs_in),
            watcher: None,
            watchers: 0,
            watched_at: None,
            opened: BTreeSet::new(),
            events: events.clone(),
        };
        viewed.shown();
        viewed
    }

    fn at(&self) -> Where {
        where_on(&self.machine, self.runs_in_here)
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
        let worktree = self.layout.worktree;
        let again = self.laid_in == Some(worktree);
        self.show = Some(lunetad::show(opened, self.at(), worktree, self.size, again, told));
    }

    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    #[must_use]
    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    #[must_use]
    pub fn worktree(&self) -> WorktreeId {
        self.layout.worktree
    }

    #[must_use]
    pub fn focused(&self) -> Option<ViewId> {
        self.layout.focused()
    }

    pub fn welcomes(&self) -> bool {
        self.layout.welcomes()
    }

    pub fn heard(&mut self, heard: Heard) -> Heeded {
        match heard {
            Heard::Laid(laid) => {
                self.laid_in = Some(laid.worktree());
                let followed = self.layout.followed(&laid);
                if followed {
                    self.opened.insert(self.layout.worktree);
                }
                let mut emptied = false;
                if self.layout.laid_out(laid) {
                    if self.opened.insert(self.layout.worktree) {
                        self.ask(Asked::Open { beside: Beside::Alone, argv: Vec::new() });
                    } else {
                        emptied = true;
                    }
                }
                self.watch();
                match (emptied, followed) {
                    (true, _) => Heeded::Emptied,
                    (false, true) => Heeded::Followed,
                    (false, false) => Heeded::Quiet,
                }
            },
            Heard::Edged { worktree, direction } => {
                self.layout.edged(worktree, direction).map_or(Heeded::Quiet, Heeded::Edged)
            },
            Heard::Sent(worktree) => Heeded::Sent(worktree),
            Heard::Left(worktree) => {
                self.opened.remove(&worktree);
                if !self.layout.left(worktree) {
                    return Heeded::Quiet;
                }
                self.watch();
                Heeded::Emptied
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
        let wanted = self.layout.wanted();
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
                let reach = match self.reach() {
                    Ok(reach) => reach,
                    Err(why) => {
                        let _ = events.send(Event::Seen { number, watcher, seen: Seen::Lost(why) });
                        return;
                    },
                };
                let greeting = self.greeting;
                let told = move |seen| drop(events.send(Event::Seen { number, watcher, seen }));
                let viewer = viewer::start(reach, greeting, told);
                viewer.show(wanted.clone());
                self.watcher = Some(Watcher { number: watcher, viewer, showing: wanted });
            },
        }
    }

    fn reach(&self) -> Result<Reach, String> {
        let destination = match self.at() {
            Where::Here { runs_in } => {
                let socket = lunetad::mux::own::server::socket();
                let socket = socket.map_err(|failure| failure.to_string())?;
                return Ok(Reach::Mux { socket, runs_in });
            },
            Where::On(destination) => destination,
        };
        match folder::open(&self.folder) {
            Ok(Some(lunetad)) => Ok(Reach::On { lunetad, destination }),
            Ok(None) => Err("lunetad has made no folder yet".to_owned()),
            Err(refusal) => Err(refusal.to_string()),
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
                return Heeded::Note(format!("luneta's viewer of luneta-mux: {why}"));
            },
        };
        let frame = framed.frame();
        if !watching.showing.contains(&frame.terminal()) {
            return Heeded::Quiet;
        }
        match self.layout.framed(frame) {
            Ok(()) => Heeded::Quiet,
            Err(why) => {
                self.unwatch();
                Heeded::Note(format!("luneta-mux said what luneta cannot follow: {why}"))
            },
        }
    }

    fn unwatch(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.viewer.give_up("luneta lets go of its viewer".to_owned());
        }
        self.layout.screens.clear();
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

    pub fn go(&mut self, on: &Machine, worktree: WorktreeId) {
        if *on != self.machine {
            let (number, folder, on) = (self.number + 1, self.folder.clone(), on.clone());
            let (size, greeting, here) = (self.size, self.greeting, self.runs_in_here);
            let events = self.events.clone();
            *self = Viewed::start(number, folder, on, worktree, size, greeting, here, &events);
            return;
        }
        if worktree == self.layout.worktree {
            return;
        }
        self.opened.remove(&worktree);
        self.layout.go(worktree);
        self.ask(Asked::Go(worktree));
        self.watch();
    }

    pub fn open_in(&mut self, worktree: WorktreeId, argv: Vec<String>) {
        if worktree != self.layout.worktree {
            return;
        }
        self.opened.insert(worktree);
        self.ask(Asked::Open { beside: Beside::Largest, argv });
    }

    pub fn welcome(&mut self) {
        self.opened.insert(self.layout.worktree);
        self.ask(Asked::Welcome);
    }

    pub fn open_beside_focused(&mut self, argv: Vec<String>) {
        self.opened.insert(self.layout.worktree);
        self.ask(Asked::Open { beside: Beside::Focused, argv });
    }

    pub fn tabs(&mut self, tabs: Vec<Tab>, titles: BTreeMap<ViewId, String>) {
        self.layout.tabs = tabs;
        self.layout.titles = titles;
    }

    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) {
        let size = layout_size(columns, rows);
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
        if let Some(key) = self.layout.focused_key() {
            self.type_in(key, event);
        }
    }

    pub fn key(&mut self, key: &KeyEvent, input: u64) {
        self.unselect();
        if self.layout.held_after_close(key) {
            return;
        }
        if self.layout.welcomes() {
            if let Some(view) = self.layout.closes(key, input) {
                self.ask(Asked::Close(view));
            }
            return;
        }
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

    pub fn mouse(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Option<WorktreeId> {
        match self.layout.moused(mouse, at, width) {
            Moused::Nothing => None,
            Moused::Go(worktree) => Some(worktree),
            Moused::Focus(terminal) => {
                self.ask(Asked::Focus(terminal));
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
        self.layout.selects(mouse, at)
    }

    pub fn select_from(&mut self, at: Spot) {
        self.layout.select_from(at);
    }

    pub fn select_to(&mut self, at: Spot) {
        self.layout.select_to(at);
    }

    pub fn selected(&mut self) -> Option<String> {
        self.layout.selected()
    }

    pub fn unselect(&mut self) {
        self.layout.pointer = self.layout.pointer.unselected();
    }

    #[must_use]
    pub fn draw(&self, buffer: &mut Buffer, area: Rect, hover: Option<Spot>) -> Option<Position> {
        self.layout.draw(buffer, area, hover)
    }

    #[must_use]
    pub fn cursor_style(&self) -> (CursorShape, bool) {
        self.layout.cursor_style()
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use crossterm::event::{KeyModifiers, MouseButton};
    use lunetad::api::{show_request, show_response};
    use lunetad::mux::own::frames::{Cursor, Ending, Glyph, Grapheme, Line, Shows};
    use lunetad::mux::own::OwnMade;
    use ratatui::style::Modifier;

    use super::*;
    use crate::attached::At;

    fn side(count: u16) -> NonZeroU16 {
        NonZeroU16::new(count).unwrap()
    }

    fn narrow(it: &str) -> Glyph {
        let shows = Shows::Narrow(Grapheme::new(it.to_owned()).unwrap());
        Glyph { shows, style: frames::Style::default() }
    }

    fn lines(rows: &[&str]) -> Vec<Line> {
        let line = |row: &str| {
            let glyphs: Vec<Glyph> = row.chars().map(|it| narrow(&it.to_string())).collect();
            Line::new(glyphs, Ending::Here, side(u16::try_from(row.len()).unwrap())).unwrap()
        };
        rows.iter().map(|row| line(row)).collect()
    }

    fn key(id: u32) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(0) }
    }

    fn whole(id: u32, rows: &[&str], cursor: Option<Cursor>) -> Frame {
        let lines = lines(rows);
        let columns = side(u16::try_from(rows[0].len()).unwrap());
        let size = frames::Size { columns, rows: side(u16::try_from(lines.len()).unwrap()) };
        Frame::new(key(id), size, Rows::Whole(lines), cursor, false).unwrap()
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    fn terminal(id: u64) -> ViewId {
        ViewId::of(id).unwrap()
    }

    fn view(terminal: u64, rect: (u32, u32, u32, u32), found: Option<u32>) -> show_response::View {
        use show_response::view::Found as Said;
        let (column, row, columns, rows) = rect;
        let found = match found {
            Some(id) => Said::Key(api_key(id)),
            None => Said::Lost(()),
        };
        show_response::View {
            id: terminal,
            rect: Some(show_response::Rect { column, row, columns, rows }),
            found: Some(found),
        }
    }

    fn api_key(id: u32) -> lunetad::api::TerminalKey {
        lunetad::api::TerminalKey::from(key(id))
    }

    fn laid(views: Vec<show_response::View>, focused: u64, full: bool) -> Laid {
        use show_response::laid::Shown as Said;
        Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size { columns: 20, rows: 4 }),
            views,
            shown: Some(if full { Said::Full(focused) } else { Said::Tiled(focused) }),
        })
        .unwrap()
    }

    fn halves() -> Vec<show_response::View> {
        vec![view(11, (0, 0, 10, 4), Some(1)), view(12, (10, 0, 10, 4), Some(2))]
    }

    fn split_layout() -> Layout {
        let mut layout = Layout::new(worktree(1), None);
        layout.tabs = vec![
            Tab { worktree: worktree(1), name: "one".to_owned() },
            Tab { worktree: worktree(2), name: "two".to_owned() },
        ];
        layout.titles = [(terminal(11), "p1".to_owned()), (terminal(12), "p2".to_owned())].into();
        assert!(!layout.laid_out(laid(halves(), 11, false)));
        let cursor =
            Cursor { row: 1, column: 2, shape: frames::CursorShape::Block, blinking: false };
        layout.framed(&whole(1, &["abcdefgh", "ijklmnop"], Some(cursor))).unwrap();
        layout.framed(&whole(2, &["qrstuvwx", "yz012345"], None)).unwrap();
        layout
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
    fn each_terminal_draws_in_its_frame_under_the_worktrees_and_a_larger_area_keeps_a_margin() {
        let layout = split_layout();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
        let cursor = layout.draw(&mut buffer, Rect::new(2, 0, 22, 6), None);
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
        assert_eq!(layout.draw(&mut small, Rect::new(0, 0, 14, 3), None), None);
        assert_eq!(row(&small, 1, 0, 14), "╭ p1 ────╮╭ p2");
        assert_eq!(row(&small, 2, 0, 14), "│abcdefgh││qrs");
    }

    #[test]
    fn a_view_of_the_worktree_left_is_not_drawn_in_the_one_gone_to() {
        let mut layout = split_layout();
        layout.go(worktree(2));
        let drawn = layout.drawn().is_empty() && layout.wanted().is_empty();
        assert!(drawn && layout.screens.is_empty());
        assert!(!layout.laid_out(laid(halves(), 11, false)));
        assert!(layout.drawn().is_empty() && layout.focused().is_none());
    }

    #[test]
    fn an_edge_is_heeded_only_from_the_worktree_the_view_is_on() {
        let mut layout = split_layout();
        assert_eq!(layout.edged(worktree(1), Direction::Right), Some(Direction::Right));
        layout.go(worktree(2));
        assert_eq!(layout.edged(worktree(1), Direction::Right), None, "an edge of the one left");
        assert_eq!(layout.edged(worktree(2), Direction::Left), Some(Direction::Left));
    }

    #[test]
    fn a_full_terminal_fills_the_view_and_a_lost_one_or_the_one_luneta_runs_in_says_so() {
        let mut layout = split_layout();
        layout.laid_out(laid(halves(), 12, true));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), None);
        assert_eq!(row(&buffer, 1, 0, 20), "╭ p2 ──────────────╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        let mut here = Layout::new(worktree(1), Some(key(2)));
        let lost = vec![view(11, (0, 0, 10, 4), None), view(12, (10, 0, 10, 4), Some(2))];
        here.laid_out(laid(lost, 11, false));
        assert_eq!(here.wanted(), BTreeSet::new(), "luneta never shows the terminal it runs in");
        here.draw(&mut buffer, Rect::new(0, 0, 20, 5), None);
        assert_eq!(row(&buffer, 2, 1, 9), "lost: lu");
        assert_eq!(row(&buffer, 2, 11, 19), "luneta r");
    }

    #[test]
    fn a_press_focuses_the_terminal_under_it_or_goes_to_its_worktree_or_reaches_the_program() {
        let mut layout = split_layout();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left));
        let up = mouse(MouseEventKind::Up(MouseButton::Left));
        let at = |moused: Moused| match moused {
            Moused::Sent(key, said) => Some((key.id.get(), said.row, said.column)),
            Moused::Nothing | Moused::Go(_) | Moused::Focus(_) => None,
        };
        assert_eq!(layout.moused(down, spot(2, 14), 22), Moused::Focus(terminal(12)));
        assert_eq!(layout.moused(down, spot(0, 7), 22), Moused::Go(worktree(2)));
        assert_eq!(layout.moused(down, spot(0, 1), 22), Moused::Nothing);
        assert_eq!(layout.moused(down, spot(1, 0), 22), Moused::Nothing);
        assert_eq!(at(layout.moused(down, spot(2, 3), 22)), Some((1, 0, 2)));
        assert_eq!(at(layout.moused(drag, spot(0, 15), 22)), Some((1, 0, 7)));
        assert_eq!(at(layout.moused(up, spot(4, 15), 22)), Some((1, 1, 7)));
        assert_eq!(layout.moused(up, spot(2, 3), 22), Moused::Nothing);
    }

    #[test]
    fn a_drag_selects_in_the_focused_terminal_and_a_new_size_lets_it_go() {
        let mut layout = split_layout();
        let press = mouse(MouseEventKind::Down(MouseButton::Left));
        assert!(layout.selects(press, spot(2, 1)));
        assert!(!layout.selects(press, spot(2, 11)));
        layout.select_from(spot(2, 1));
        layout.select_to(spot(3, 30));
        assert_eq!(layout.selected().as_deref(), Some("abcdefgh\nijklmnop"));
        assert!(matches!(layout.pointer, Pointer::Shown(_)));
        layout.framed(&whole(2, &["ab"], None)).unwrap();
        assert!(matches!(layout.pointer, Pointer::Shown(_)));
        layout.framed(&whole(1, &["ab"], None)).unwrap();
        assert_eq!(layout.pointer, Pointer::Free);
    }

    #[test]
    fn a_click_on_a_link_copies_it_and_the_pointer_over_one_underlines_it() {
        let mut layout = split_layout();
        layout.framed(&whole(1, &["a/b.rs x", "ijklmnop"], None)).unwrap();
        layout.select_from(spot(2, 3));
        assert_eq!(layout.selected().as_deref(), Some("a/b.rs"));
        let lit = Selection { from: At { row: 0, column: 0 }, to: At { row: 0, column: 5 } };
        assert_eq!(layout.pointer, Pointer::Shown(lit));
        layout.select_from(spot(2, 3));
        layout.select_to(spot(2, 3));
        assert_eq!(layout.selected().as_deref(), Some("a/b.rs"), "a press that stays is a click");
        layout.select_from(spot(2, 3));
        layout.select_to(spot(2, 6));
        layout.select_to(spot(2, 3));
        assert_eq!(layout.selected(), None, "a drag that comes back is no click");
        assert_eq!(layout.pointer, Pointer::Free);
        layout.select_from(spot(2, 8));
        assert_eq!(layout.selected(), None, "a click on a word copies nothing");
        assert_eq!(layout.pointer, Pointer::Free);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), Some(spot(2, 2)));
        let underlined = |x| buffer[(x, 2)].modifier.contains(Modifier::UNDERLINED);
        let line: String = (1..9).map(|x| if underlined(x) { '_' } else { ' ' }).collect();
        assert_eq!(line, "______  ");
        assert_eq!(layout.pointed(spot(2, 13)), None, "only in the focused terminal");
        let size = frames::Size { columns: side(8), rows: side(2) };
        let rows = Rows::Whole(lines(&["a/b.rs x", "ijklmnop"]));
        layout.framed(&Frame::new(key(1), size, rows, None, true).unwrap()).unwrap();
        let taken = "where the program takes the mouse, none shows";
        assert_eq!(layout.pointed(spot(2, 2)), None, "{taken}");
        layout.framed(&whole(1, &["x a/b.rs"], None)).unwrap();
        let past = "past a screen shorter than its view, none shows";
        assert_eq!(layout.pointed(spot(3, 8)), None, "{past}");
        layout.select_from(spot(3, 8));
        assert_eq!(layout.selected(), None, "nor does a click there copy");
        assert_eq!(layout.pointer, Pointer::Free);
        layout.select_from(spot(3, 8));
        layout.select_to(spot(2, 3));
        assert_eq!(layout.selected().as_deref(), Some("a/b.rs"), "a drag from there still selects");
    }

    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size_and_a_terminal_laid_out_no_more_drops() {
        let mut layout = split_layout();
        let changed = Rows::Changed(vec![(0, lines(&["c"]).remove(0))]);
        let size = frames::Size { columns: side(1), rows: side(1) };
        let frame = Frame::new(key(1), size, changed, None, false).unwrap();
        let why = layout.framed(&frame).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 8x2");
        layout.laid_out(laid(vec![view(12, (0, 0, 20, 4), Some(2))], 12, false));
        assert_eq!(layout.screens.keys().copied().collect::<Vec<_>>(), [key(2)]);
        layout.framed(&whole(1, &["no"], None)).unwrap();
        let lacked = "a terminal its layout lacks gets no screen";
        assert!(!layout.screens.contains_key(&key(1)), "{lacked}");
        layout.go(worktree(2));
        assert_eq!(layout.focused(), None, "a layout of another worktree lays out nothing here");
    }

    fn laid_in(id: u64, views: Vec<show_response::View>) -> Heard {
        use show_response::laid::Shown as Said;
        let shown = views.first().map(|view| Said::Tiled(view.id));
        let size = Some(show_request::Size { columns: 20, rows: 4 });
        let said = show_response::Laid { worktree: id, size, views, shown };
        Heard::Laid(Laid::heard(said).unwrap())
    }

    #[test]
    fn d_twice_in_a_row_closes_the_welcome_view_and_anything_between_starts_over() {
        use show_response::view::Found as Said;
        let welcome = |at: (u32, u32, u32, u32)| {
            let mut it = view(12, at, None);
            it.found = Some(Said::Welcome(()));
            it
        };
        let views = || vec![view(11, (0, 0, 10, 4), Some(1)), welcome((10, 0, 10, 4))];
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid(views(), 12, false)));
        let typed = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let d = typed(KeyCode::Char(CLOSE));
        let welcome = Some(ViewId::of(12).unwrap());
        assert_eq!(layout.closes(&d, 1), None);
        assert_eq!(layout.closes(&d, 2), welcome, "d d closes it, by its id");
        assert_eq!(layout.closes(&d, 3), None);
        assert_eq!(layout.closes(&typed(KeyCode::Char('x')), 4), None);
        assert_eq!(layout.closes(&d, 5), None, "another key between the two starts over");
        let ctrl_d = KeyEvent::new(KeyCode::Char(CLOSE), KeyModifiers::CONTROL);
        assert_eq!(layout.closes(&ctrl_d, 6), None);
        assert_eq!(layout.closes(&d, 7), None, "Ctrl d is no d");
        assert!(!layout.laid_out(laid(views(), 11, false)));
        assert!(!layout.laid_out(laid(views(), 12, false)));
        assert_eq!(layout.closes(&d, 8), None, "a focus gone elsewhere and back starts over");
        assert_eq!(layout.closes(&d, 9), welcome);
        assert_eq!(layout.closes(&d, 10), None);
        let elsewhere = "an input luneta takes itself between the two starts over";
        assert_eq!(layout.closes(&d, 12), None, "{elsewhere}");
        assert_eq!(layout.closes(&d, 13), welcome);
    }

    #[test]
    fn a_held_d_closes_no_welcome_view_and_after_d_d_reaches_no_view() {
        use show_response::view::Found as Said;
        let mut welcome = view(12, (10, 0, 10, 4), None);
        welcome.found = Some(Said::Welcome(()));
        let views = || vec![view(11, (0, 0, 10, 4), Some(1)), welcome];
        let mut layout = Layout::new(worktree(1), None);
        layout.laid_out(laid(views(), 12, false));
        let kind = |kind| KeyEvent::new_with_kind(KeyCode::Char(CLOSE), KeyModifiers::NONE, kind);
        let (d, held) = (kind(KeyEventKind::Press), kind(KeyEventKind::Repeat));
        assert_eq!(layout.closes(&d, 1), None);
        for input in 2..6 {
            assert!(!layout.held_after_close(&held));
            assert_eq!(layout.closes(&held, input), None, "a held d is no d d");
        }
        assert_eq!(layout.closes(&d, 6), None);
        assert_eq!(layout.closes(&d, 7), Some(ViewId::of(12).unwrap()));
        assert!(!layout.laid_out(laid(vec![view(11, (0, 0, 20, 4), Some(1))], 11, false)));
        assert!(layout.held_after_close(&held), "the d held after the close is dropped");
        assert!(layout.held_after_close(&held));
        assert!(!layout.held_after_close(&d), "a d pressed again reaches the terminal");
        assert!(!layout.held_after_close(&held));
    }

    #[test]
    fn a_worktree_emptied_is_left_and_one_gone_back_to_opens_again() {
        let unserved = tempfile::tempdir().unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(side(20), side(6));
        let greeting = Greeting { foreground: None, background: None };
        let folder = unserved.path().to_owned();
        let mut viewed =
            Viewed::start(1, folder, Machine::Local, worktree(1), size, greeting, None, &events);
        let one = || vec![view(11, (0, 0, 20, 4), Some(1))];
        assert!(matches!(viewed.heard(laid_in(1, Vec::new())), Heeded::Quiet), "first, it opens");
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Quiet));
        let emptied = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(emptied, Heeded::Emptied), "its last terminal closed, luneta leaves it");
        viewed.go(&Machine::Local, worktree(2));
        let two = vec![view(21, (0, 0, 20, 4), Some(2))];
        assert!(matches!(viewed.heard(laid_in(2, two)), Heeded::Quiet));
        viewed.go(&Machine::Local, worktree(1));
        let back = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(back, Heeded::Quiet), "gone back to, it opens as it did first");
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Quiet));
        let again = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(again, Heeded::Emptied), "emptied again, luneta leaves it again");
    }
}
