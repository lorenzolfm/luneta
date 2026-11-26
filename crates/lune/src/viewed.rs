use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::num::{NonZeroU16, NonZeroU64};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{Arranged, BrowserUrl, Opens, ProjectId, Size, ViewId, WorktreeId};
use luned::folder;
use luned::log;
use luned::mux::own::api::{self, typed};
use luned::mux::own::frames::{self, Frame, Rows};
use luned::mux::own::viewer::{self, Greeting, Reach, Seen, Viewer};
use luned::{
    Asked, Beside, Direction, Found, Heard, Laid, View, Rect as Placed, Show, Shows, Where,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::answers::{CellPixels, Colours};
use crate::attached::{
    below_bar, border, draw_screen, draw_tab_spans, key_said, link, mouse_said, on_views,
    screen_rect, selects, span_under, spot_in, tab_columns_max, tab_spans, tab_spans_capped,
    text, within, Pointer, Screen, Selection, Spot, TabSpan, TAB_BAR_ROWS, TAB_PAD,
};
use crate::copied::Excerpt;
use crate::desktop;
use crate::embedded::{self, Browsers, Desired, Outcome, PointerShape, Shown};
use crate::host::Event;
use crate::machines::Machine;
use crate::moonlight;
use crate::sidebar::colour_of;

const AGAIN: Duration = Duration::from_secs(1);

const VIEW_DOT: &str = "●";

const SIDE_MAX: u16 = 1000;

const LOST: &str = "lost: lune-mux no longer holds it";

const LOOKING: &str = "looking for it in lune-mux";

const HERE: &str = "lune runs here";

const UNTITLED: &str = "terminal";

const WELCOME_TAB: &str = "welcome";

const BROWSER_TAB: &str = "browser";

const WELCOME: [&str; 4] =
    ["Welcome", "", "This view runs no terminal.", "d d  close it   Ctrl hjkl  another view"];

const BROWSER_UNSUPPORTED_TERMINAL: &str = "This terminal does not support Kitty graphics.";

const BROWSER_UNSUPPORTED_SSH: &str = "Inline browser graphics are unavailable over SSH.";

const BROWSER_FALLBACK_HELP: &str =
    "o  open in OS browser   y  copy URL   d d  close it   Ctrl hjkl  another view";

const BROWSER_RETRY_HELP: &str =
    "r  retry   o  open in OS browser   y  copy URL   d d  close it   Ctrl hjkl  another view";

const CLOSE: char = 'd';

const UNSENT_MAX: usize = 256;

const UNSENT_FOR: Duration = Duration::from_secs(10);

const _: () = assert!(UNSENT_MAX <= viewer::SAID_MAX / 4);

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeTabs {
    pub project: ProjectId,
    pub tabs: Vec<Tab>,
}

#[derive(Debug, PartialEq, Eq)]
enum Moused {
    Nothing,
    Go(WorktreeId),
    Focus(ViewId),
    Sent(TerminalKey, api::Mouse),
    Browser { view: ViewId, mouse: MouseEvent, column: u16, row: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Across {
    Before,
    After,
}

pub enum Heeded {
    Quiet,
    Followed,
    Edged(Across),
    Sent(WorktreeId),
    Note(String),
    Copy(String),
    OpenOsBrowser(BrowserUrl),
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
    worktree_tabs: Option<WorktreeTabs>,
    titles: BTreeMap<ViewId, String>,
    closing: Closing,
    unsent: Unsent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Aim {
    worktree: WorktreeId,
    view: Option<ViewId>,
    key: Option<TerminalKey>,
}

impl Aim {
    fn reaches(self, now: Aim) -> bool {
        let view = self.view.is_none_or(|view| Some(view) == now.view);
        let key = self.key.is_none_or(|key| Some(key) == now.key);
        self.worktree == now.worktree && view && key
    }
}

struct Unsent {
    aim: Aim,
    held: Held,
}

enum Held {
    Nothing,
    Kept(Kept),
    Dropping,
}

struct Kept {
    since: Instant,
    events: VecDeque<typed::Event>,
    dropped: bool,
}

impl Unsent {
    fn new(aim: Aim) -> Unsent {
        Unsent { aim, held: Held::Nothing }
    }

    fn aim_at(&mut self, aim: Aim, now: Instant) {
        let moved = !self.aim.reaches(aim);
        self.aim = aim;
        if moved {
            if let Held::Kept(kept) = std::mem::replace(&mut self.held, Held::Nothing) {
                let count = kept.events.len();
                let dropped = format!("lune drops {count} events kept for a terminal yet to come");
                log::say(&format!("{dropped}: typed for a terminal no longer focused"));
            }
            return;
        }
        let Held::Kept(kept) = &self.held else {
            return;
        };
        if now.saturating_duration_since(kept.since) <= UNSENT_FOR {
            return;
        }
        let count = kept.events.len();
        self.held = Held::Dropping;
        let dropped = format!("lune drops {count} events kept for a terminal yet to come");
        let after = "and what is typed after them till a terminal takes it";
        log::say(&format!("{dropped}: typed over {UNSENT_FOR:?} ago, {after}"));
    }

    fn keep(&mut self, aim: Aim, event: typed::Event, now: Instant) {
        self.aim_at(aim, now);
        let kept = match &mut self.held {
            Held::Nothing => {
                let events = VecDeque::from([event]);
                self.held = Held::Kept(Kept { since: now, events, dropped: false });
                return;
            },
            Held::Kept(kept) => kept,
            Held::Dropping => return,
        };
        if kept.events.len() < UNSENT_MAX {
            kept.events.push_back(event);
            assert!(kept.events.len() <= UNSENT_MAX);
            return;
        }
        if !kept.dropped {
            let most = format!("lune keeps at most {UNSENT_MAX} events for a terminal yet to come");
            log::say(&format!("{most}: later input dropped"));
        }
        kept.dropped = true;
    }

    fn clear(&mut self) {
        self.held = Held::Nothing;
    }

    fn take(&mut self) -> VecDeque<typed::Event> {
        match std::mem::replace(&mut self.held, Held::Nothing) {
            Held::Kept(kept) => kept.events,
            Held::Nothing | Held::Dropping => VecDeque::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Typing {
    Into(TerminalKey),
    Later,
    Never,
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
            worktree_tabs: None,
            titles: BTreeMap::new(),
            closing: Closing::Unarmed,
            unsent: Unsent::new(Aim { worktree, view: None, key: None }),
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

    fn browses(&self) -> bool {
        self.laid()
            .and_then(Laid::focused)
            .is_some_and(|view| matches!(view.found, Found::Browser { .. }))
    }

    fn focused_browser(&self) -> Option<(ViewId, Option<&BrowserUrl>, Rect)> {
        let (view, placed) = self.laid()?.focused_at()?;
        let Found::Browser { url } = &view.found else {
            return None;
        };
        let inner = screen_rect(view_rect(Rect::default(), placed));
        Some((view.id, url.as_ref(), inner))
    }

    fn closes(&mut self, key: &KeyEvent, input: u64) -> Option<ViewId> {
        assert!(self.welcomes() || self.browses(), "only a welcome or browser view closes by keys");
        let focused = self.focused().expect("a welcome or browser view is focused");
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
        match self.typing() {
            Typing::Into(key) => Some(key),
            Typing::Later | Typing::Never => None,
        }
    }

    fn typing(&self) -> Typing {
        let Some(focused) = self.laid().and_then(Laid::focused) else {
            return Typing::Later;
        };
        match &focused.found {
            Found::Key(key) if Some(*key) == self.runs_in => Typing::Never,
            Found::Key(key) => Typing::Into(*key),
            Found::Looking => Typing::Later,
            Found::Lost | Found::Welcome | Found::Browser { .. } => Typing::Never,
        }
    }

    fn aim(&self) -> Aim {
        let focused = self.laid().and_then(Laid::focused);
        let (view, key) = (focused.map(|view| view.id), focused.and_then(View::key));
        Aim { worktree: self.worktree, view, key }
    }

    fn keep_unsent(&mut self, event: typed::Event, now: Instant) {
        let aim = self.aim();
        self.unsent.keep(aim, event, now);
    }

    fn take_unsent<V>(
        &mut self,
        viewer: Option<V>,
        now: Instant,
    ) -> Option<(V, TerminalKey, VecDeque<typed::Event>)> {
        let aim = self.aim();
        self.unsent.aim_at(aim, now);
        let key = match self.typing() {
            Typing::Into(key) => key,
            Typing::Later => return None,
            Typing::Never => {
                self.unsent.clear();
                return None;
            },
        };
        assert_ne!(Some(key), self.runs_in, "lune types nothing into its own terminal");
        let viewer = viewer?;
        Some((viewer, key, self.unsent.take()))
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

    fn edged(&self, worktree: WorktreeId, direction: Direction) -> Option<Across> {
        if worktree != self.worktree {
            return None;
        }
        match direction {
            Direction::Left => Some(Across::Before),
            Direction::Right => Some(Across::After),
            Direction::Up | Direction::Down => None,
        }
    }

    fn named_tabs(&self) -> impl Iterator<Item = (WorktreeId, &str)> {
        let tabs = self.worktree_tabs.iter().flat_map(|it| &it.tabs);
        tabs.map(|tab| (tab.worktree, tab.name.as_str()))
    }

    fn view_tabs(&self, outer: Rect, browsers: Option<&Browsers>) -> Vec<TabSpan<ViewId>> {
        let Some(laid) = self.laid().filter(|laid| laid.shows() == Some(Shows::Tabs)) else {
            return Vec::new();
        };
        let width = outer.width.saturating_sub(2);
        if width == 0 {
            return Vec::new();
        }
        let focused = laid.focused().expect("a layout shown as tabs focuses one").id;
        let named = laid.in_order().map(|view| {
            let title = self.view_title(view, browsers);
            (view.id, title)
        });
        let named: Vec<(ViewId, &str)> = named.collect();
        let from_each = (0..named.len()).map(|hidden| {
            let shown = &named[hidden..];
            tab_spans_capped(shown.iter().copied(), width, tab_columns_max(shown, width))
        });
        let mut drawn = from_each;
        let drawn = drawn.find(|spans| spans.iter().any(|span| span.id == focused));
        drawn.expect("the focused tab is drawn on a border of a column or more")
    }

    fn view_title<'a>(&'a self, view: &'a View, browsers: Option<&'a Browsers>) -> &'a str {
        match &view.found {
            Found::Browser { url } => browsers
                .and_then(|browsers| browsers.title(view.id))
                .filter(|title| !title.is_empty())
                .or_else(|| url.as_ref().map(BrowserUrl::short))
                .unwrap_or(BROWSER_TAB),
            Found::Welcome => WELCOME_TAB,
            Found::Key(_) | Found::Lost | Found::Looking => self
                .titles
                .get(&view.id)
                .map(String::as_str)
                .filter(|it| !it.is_empty())
                .unwrap_or(UNTITLED),
        }
    }

    fn tab_under(&self, at: Position, browsers: Option<&Browsers>) -> Option<ViewId> {
        let (_, placed) = self.laid()?.focused_at()?;
        let outer = view_rect(Rect::default(), placed);
        if at.y == outer.y {
            let column = at.x.checked_sub(outer.x)?.checked_sub(1)?;
            span_under(&self.view_tabs(outer, browsers), column)
        } else {
            None
        }
    }

    fn moused(
        &mut self,
        mouse: MouseEvent,
        at: Spot,
        width: u16,
        browsers: Option<&Browsers>,
    ) -> Moused {
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
        let under = self.tab_under(spot, browsers).or_else(|| {
            let drawn = self.drawn().into_iter();
            let mut under =
                drawn.filter(|(_, placed)| view_rect(Rect::default(), *placed).contains(spot));
            under.next().map(|(view, _)| view.id)
        });
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
        if let Some((view, _, inner)) = self.focused_browser()
            && !inner.is_empty()
        {
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
            if sends {
                let spot = spot_in(inner, at);
                return Moused::Browser { view, mouse, column: spot.column, row: spot.row };
            }
            return Moused::Nothing;
        }
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

    fn selected(&mut self) -> Option<Excerpt> {
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let chosen = match self.pointer {
            Pointer::Pressed(at) => screen.and_then(|screen| link(screen, at)),
            Pointer::Selecting(selection) if selection.from != selection.to => screen
                .and_then(|screen| Some((selection, Excerpt::of(text(screen, selection))?))),
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

    fn draw(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        hover: Option<Spot>,
        browsers: Option<&Browsers>,
    ) -> Option<Position> {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let bar = Rect { height: area.height.min(TAB_BAR_ROWS), ..area };
        let views = Rect { y: area.y + bar.height, height: area.height - bar.height, ..area };
        let spans = tab_spans(self.named_tabs(), bar.width);
        let worktree_tabs = self.worktree_tabs.as_ref();
        let colour = worktree_tabs.map_or(moonlight::GREEN, |it| colour_of(it.project));
        draw_tab_spans(buffer, bar, &spans, self.worktree, colour);
        let focused = self.focused();
        let selection = self.pointer.selection();
        let pointed = hover.and_then(|at| self.pointed(at));
        let mut cursor = None;
        for (view, placed) in self.drawn() {
            let mine = Some(view.id) == focused;
            let outer = view_rect(views, placed);
            let edge = if mine { moonlight::MUTE } else { moonlight::LIFTED };
            let named = if mine {
                Style::new().fg(moonlight::TEXT).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(moonlight::MUTE)
            };
            let tabs = self.view_tabs(outer, browsers);
            let titled = match &view.found {
                Found::Browser { .. } => self.view_title(view, browsers),
                _ => self.titles.get(&view.id).map_or("", String::as_str),
            };
            let title = if tabs.is_empty() { titled } else { "" };
            border(buffer, views, outer, title, Style::new().fg(edge), named);
            draw_view_tabs(buffer, views, outer, &tabs, view.id, colour);
            let seen = screen_rect(outer).intersection(views);
            if seen.is_empty() {
                continue;
            }
            let said = match &view.found {
                Found::Key(key) if Some(*key) == self.runs_in => HERE,
                Found::Key(key) => {
                    if let Some(screen) = self.screens.get(key) {
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
                    let style = Style::new().fg(if mine { colour } else { moonlight::MUTE });
                    for (line, y) in WELCOME.iter().zip(seen.top()..seen.bottom()) {
                        buffer.set_stringn(seen.x, y, line, usize::from(seen.width), style);
                    }
                    continue;
                },
                Found::Browser { url } => {
                    draw_browser(buffer, seen, view.id, url.as_ref(), mine, colour, browsers);
                    continue;
                },
            };
            let style = Style::new().fg(moonlight::MUTE);
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

fn draw_view_tabs(
    buffer: &mut Buffer,
    views: Rect,
    outer: Rect,
    tabs: &[TabSpan<ViewId>],
    focused: ViewId,
    colour: Color,
) {
    let start = outer.x.checked_add(1).expect("a view's border is a terminal column");
    for span in tabs {
        let x = start + span.column;
        if !views.contains(Position::new(x, outer.y)) {
            continue;
        }
        let lit = Rect::new(x, outer.y, span.columns, 1).intersection(views);
        let (name, columns) = (span.name(), usize::from(span.columns));
        let rest = |lead: &str| " ".repeat(columns.saturating_sub(lead.width() + name.width()));
        let said = if span.id == focused {
            let edge = " ".repeat(usize::from(TAB_PAD - 1));
            let lead = format!("{edge}{VIEW_DOT} ");
            let named = Style::new().fg(moonlight::TEXT).add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::raw(edge),
                Span::styled(VIEW_DOT, Style::new().fg(colour)),
                Span::raw(" "),
                Span::styled(name, named),
                Span::raw(rest(&lead)),
            ])
        } else {
            let pad = " ".repeat(usize::from(TAB_PAD));
            Line::styled(format!("{pad}{name}{}", rest(&pad)), Style::new().fg(moonlight::MUTE))
        };
        if columns >= 2 * usize::from(TAB_PAD) {
            assert_eq!(said.width(), columns, "a tab paints the columns it was given");
        }
        buffer.set_line(x, outer.y, &said, lit.width);
    }
}

fn paint_lines(buffer: &mut Buffer, seen: Rect, lines: &[(&str, Style)]) {
    assert!((1..=8).contains(&lines.len()), "browser status panel paints 1 to 8 lines");
    for (&(line, style), y) in lines.iter().zip(seen.top()..seen.bottom()) {
        buffer.set_stringn(seen.x, y, line, usize::from(seen.width), style);
    }
}

fn draw_browser(
    buffer: &mut Buffer,
    seen: Rect,
    view: ViewId,
    url: Option<&BrowserUrl>,
    mine: bool,
    colour: Color,
    browsers: Option<&Browsers>,
) {
    assert!(!seen.is_empty(), "drawn browser rect is non-empty");
    let url_line = url.map_or("Start page", BrowserUrl::as_str);
    let accent = Style::new().fg(if mine { colour } else { moonlight::MUTE });
    let text_style = Style::new().fg(moonlight::TEXT);
    let dim = Style::new().fg(moonlight::MUTE);
    if let Some(browsers) = browsers
        && browsers.supported()
    {
        match browsers.draw(view, buffer, seen) {
            Shown::Live => return,
            Shown::Starting => {
                paint_lines(buffer, seen, &[
                    ("Starting browser…", accent),
                    ("", dim),
                    (url_line, text_style),
                ]);
                return;
            },
            Shown::Failed(why) => {
                paint_lines(buffer, seen, &[
                    ("Browser", accent),
                    ("", dim),
                    (url_line, text_style),
                    ("", dim),
                    (why, dim),
                    ("", dim),
                    (BROWSER_RETRY_HELP, dim),
                ]);
                return;
            },
        }
    }
    let why = if desktop::elsewhere() {
        BROWSER_UNSUPPORTED_SSH
    } else {
        BROWSER_UNSUPPORTED_TERMINAL
    };
    paint_lines(buffer, seen, &[
        ("Browser", accent),
        ("", dim),
        (url_line, text_style),
        ("", dim),
        (why, dim),
        ("", dim),
        (BROWSER_FALLBACK_HELP, dim),
    ]);
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
    browsers: Browsers,
    window_focused: bool,
    watcher: Option<Watcher>,
    watchers: u64,
    watched_at: Option<Instant>,
    opened: BTreeSet<WorktreeId>,
    steps: u64,
    awaited: Option<NonZeroU64>,
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
        browser_config: embedded::Config,
        events: &Sender<Event>,
    ) -> Viewed {
        let runs_in = where_on(&machine, runs_in_here).runs_in();
        let browsers = Browsers::new(number, browser_config, events.clone());
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
            browsers,
            window_focused: true,
            watcher: None,
            watchers: 0,
            watched_at: None,
            opened: BTreeSet::new(),
            steps: 0,
            awaited: None,
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
            Ok(None) => return lost("luned has made no folder yet".to_owned()),
            Err(refusal) => return lost(refusal.to_string()),
        };
        let events = self.events.clone();
        let told = move |heard| drop(events.send(Event::Shown(number, heard)));
        let worktree = self.layout.worktree;
        let again = self.laid_in == Some(worktree);
        self.show = Some(luned::show(opened, self.at(), worktree, self.size, again, told));
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

    pub fn browses(&self) -> bool {
        self.layout.browses()
    }

    pub fn browser(&mut self, view: ViewId, heard: embedded::Heard) -> Heeded {
        match self.browsers.heard(view, heard) {
            Some(Outcome::Copy(text)) => Heeded::Copy(text),
            Some(Outcome::Navigated { view, url }) => {
                self.ask(Asked::Navigate { view, url });
                Heeded::Quiet
            },
            Some(Outcome::Close(view)) => {
                self.ask(Asked::Close(view));
                Heeded::Quiet
            },
            None => Heeded::Quiet,
        }
    }

    pub fn colours(&mut self, colours: Colours) {
        self.browsers.colours(colours);
        assert_eq!(self.browsers.config().colours, colours);
    }

    pub fn cell_resized(&mut self, cell: CellPixels) {
        assert!(cell.width > 0 && cell.height > 0, "cell dimensions are positive");
        self.browsers.cell_resized(cell);
        assert_eq!(self.browsers.config().cell, cell);
    }

    pub fn drain_graphics(&mut self) -> Vec<u8> {
        self.browsers.drain_graphics()
    }

    pub fn drain_all_deletes(&mut self) -> Vec<u8> {
        self.browsers.drain_all_deletes()
    }

    #[must_use]
    pub fn pointer_shape(&self, hover: Option<Spot>) -> Option<&PointerShape> {
        let at = on_views(hover?)?;
        for (view, placed) in self.layout.drawn() {
            if !matches!(view.found, Found::Browser { .. }) {
                continue;
            }
            let inner = screen_rect(view_rect(Rect::default(), placed));
            if inner.contains(at) {
                return self.browsers.pointer(view.id);
            }
        }
        None
    }

    fn sync_browsers(&mut self) {
        let Some(laid) = self.layout.laid() else {
            self.browsers.clear();
            return;
        };
        let drawn: BTreeMap<ViewId, Placed> =
            laid.drawn().into_iter().map(|(view, placed)| (view.id, placed)).collect();
        let desired: Vec<Desired<'_>> = laid
            .in_order()
            .filter_map(|view| {
                let Found::Browser { url } = &view.found else {
                    return None;
                };
                let (placed, visible) = match drawn.get(&view.id) {
                    Some(&placed) => (placed, true),
                    None => (view.rect, false),
                };
                let inner = screen_rect(view_rect(Rect::default(), placed));
                Some(Desired { view: view.id, url: url.as_ref(), inner, visible })
            })
            .collect();
        assert!(
            desired.len() <= lune_domain::projects::LAYOUT_VIEWS_MAX,
            "desired browsers stay within LAYOUT_VIEWS_MAX"
        );
        self.browsers.sync(
            &self.machine,
            &desired,
            laid.focused().map(|view| view.id),
            self.window_focused,
        );
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
                        self.ask(Asked::Open { beside: Beside::Alone, opens: Opens::Shell });
                    } else {
                        emptied = true;
                    }
                }
                self.watch();
                self.sync_browsers();
                self.send_unsent();
                match (emptied, followed) {
                    (true, _) => Heeded::Emptied,
                    (false, true) => Heeded::Followed,
                    (false, false) => Heeded::Quiet,
                }
            },
            Heard::Edged { worktree, direction, step } => {
                let awaited = self.awaited.take_if(|awaited| *awaited == step).is_some();
                match self.layout.edged(worktree, direction) {
                    Some(across) if awaited => Heeded::Edged(across),
                    Some(_) | None => Heeded::Quiet,
                }
            },
            Heard::Sent(worktree) => Heeded::Sent(worktree),
            Heard::Left(worktree) => {
                self.opened.remove(&worktree);
                if !self.layout.left(worktree) {
                    return Heeded::Quiet;
                }
                self.watch();
                self.sync_browsers();
                Heeded::Emptied
            },
            Heard::Refused(why) => Heeded::Note(format!("luned refused: {why}")),
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
                let socket = luned::mux::own::server::socket();
                let socket = socket.map_err(|failure| failure.to_string())?;
                return Ok(Reach::Mux { socket, runs_in });
            },
            Where::On(destination) => destination,
        };
        match folder::open(&self.folder) {
            Ok(Some(luned)) => Ok(Reach::On { luned, destination }),
            Ok(None) => Err("luned has made no folder yet".to_owned()),
            Err(refusal) => Err(refusal.to_string()),
        }
    }

    pub fn seen(&mut self, watcher: u64, seen: Seen) -> Heeded {
        let Some(watching) = self.watcher.as_ref().filter(|it| it.number == watcher) else {
            return Heeded::Quiet;
        };
        let framed = match seen {
            Seen::Frame(framed) => framed,
            Seen::Refused(why) => return Heeded::Note(format!("lune-mux refused: {why}")),
            Seen::Lost(why) => {
                self.unwatch();
                return Heeded::Note(format!("lune's viewer: {why}"));
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
                Heeded::Note(format!("lune-mux said what lune cannot follow: {why}"))
            },
        }
    }

    fn unwatch(&mut self) {
        self.watcher = None;
        self.layout.screens.clear();
    }

    pub fn ticked(&mut self) {
        if self.show.is_none() && self.shown_at.elapsed() >= AGAIN {
            self.shown();
        }
        if self.watcher.is_none() {
            self.watch();
            self.send_unsent();
        }
    }

    pub fn ask(&mut self, asked: Asked) {
        if matches!(asked, Asked::Focus(_) | Asked::Go(_)) {
            self.awaited = None;
        }
        if let Some(show) = &self.show {
            show.ask(asked);
        }
    }

    pub fn step(&mut self, direction: Direction) {
        self.steps += 1;
        let step = NonZeroU64::new(self.steps).expect("steps count up from 1");
        self.ask(Asked::Step { direction, step });
        self.awaited = Some(step);
    }

    pub fn forget_step(&mut self) {
        self.awaited = None;
    }

    pub fn go(&mut self, on: &Machine, worktree: WorktreeId) -> Vec<u8> {
        if *on != self.machine {
            let deletes = self.browsers.drain_all_deletes();
            let (number, folder, on) = (self.number + 1, self.folder.clone(), on.clone());
            let (size, greeting, here) = (self.size, self.greeting, self.runs_in_here);
            let (browser_config, events) = (self.browsers.config().clone(), self.events.clone());
            *self = Viewed::start(
                number,
                folder,
                on,
                worktree,
                size,
                greeting,
                here,
                browser_config,
                &events,
            );
            return deletes;
        }
        if worktree == self.layout.worktree {
            return Vec::new();
        }
        self.opened.remove(&worktree);
        self.layout.go(worktree);
        self.browsers.clear();
        self.ask(Asked::Go(worktree));
        self.watch();
        self.browsers.drain_all_deletes()
    }

    pub fn welcome(&mut self) {
        self.opened.insert(self.layout.worktree);
        self.ask(Asked::Welcome);
    }

    pub fn open_browser(&mut self, url: Option<BrowserUrl>) {
        self.opened.insert(self.layout.worktree);
        self.ask(Asked::Browser(url));
    }

    pub fn arrange(&mut self, arranged: Arranged) {
        self.ask(Asked::Arrange(arranged));
    }

    pub fn open_beside_focused(&mut self, opens: Opens) {
        self.opened.insert(self.layout.worktree);
        self.ask(Asked::Open { beside: Beside::Focused, opens });
    }

    pub fn tabs(&mut self, worktree_tabs: Option<WorktreeTabs>, titles: BTreeMap<ViewId, String>) {
        self.layout.worktree_tabs = worktree_tabs;
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

    fn type_focused(&mut self, event: typed::Event) {
        self.layout.keep_unsent(event, Instant::now());
        self.send_unsent();
    }

    fn send_unsent(&mut self) {
        let watcher = self.watcher.as_ref();
        let Some((watcher, key, events)) = self.layout.take_unsent(watcher, Instant::now()) else {
            return;
        };
        assert!(watcher.showing.contains(&key), "a watcher shows each terminal laid out");
        for event in events {
            watcher.viewer.typed(key, event);
        }
    }

    pub fn key(&mut self, key: &KeyEvent, input: u64) -> Heeded {
        self.unselect();
        if self.layout.held_after_close(key) {
            return Heeded::Quiet;
        }
        if self.layout.welcomes() {
            if let Some(view) = self.layout.closes(key, input) {
                self.ask(Asked::Close(view));
            }
            return Heeded::Quiet;
        }
        if let Some((view, url, inner)) = self.layout.focused_browser() {
            let url = url.cloned();
            if self.browsers.key(view, key) {
                return Heeded::Quiet;
            }
            if let Some(closed) = self.layout.closes(key, input) {
                self.ask(Asked::Close(closed));
                return Heeded::Quiet;
            }
            let pressed = key.kind == KeyEventKind::Press && key.modifiers.is_empty();
            if pressed {
                match key.code {
                    KeyCode::Char('o') => {
                        if let Some(url) = url {
                            return Heeded::OpenOsBrowser(url);
                        }
                    },
                    KeyCode::Char('y') => {
                        if let Some(url) = url {
                            return Heeded::Copy(url.as_str().to_owned());
                        }
                    },
                    KeyCode::Char('r') if self.browsers.supported() => {
                        self.browsers.retry(view, &self.machine, url.as_ref(), inner, self.window_focused);
                    },
                    _ => {},
                }
            }
            return Heeded::Quiet;
        }
        if let Some(key) = key_said(key) {
            self.type_focused(typed::Event::Key(key));
        }
        Heeded::Quiet
    }

    pub fn paste(&mut self, text: &str) {
        self.unselect();
        if let Some((view, _, _)) = self.layout.focused_browser() {
            self.browsers.paste(view, text);
            return;
        }
        self.type_focused(typed::Event::Paste(api::Paste { text: text.to_owned() }));
    }

    pub fn focus(&mut self, focused: bool) {
        self.window_focused = focused;
        self.sync_browsers();
        self.type_focused(typed::Event::Focus(api::Focus { focused }));
    }

    pub fn mouse(&mut self, mouse: MouseEvent, at: Spot, width: u16) -> Option<WorktreeId> {
        match self.layout.moused(mouse, at, width, Some(&self.browsers)) {
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
            Moused::Browser { view, mouse, column, row } => {
                self.browsers.mouse(view, mouse, column, row);
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

    pub fn selected(&mut self) -> Option<Excerpt> {
        self.layout.selected()
    }

    pub fn unselect(&mut self) {
        self.layout.pointer = self.layout.pointer.unselected();
    }

    #[must_use]
    pub fn draw(&self, buffer: &mut Buffer, area: Rect, hover: Option<Spot>) -> Option<Position> {
        self.layout.draw(buffer, area, hover, Some(&self.browsers))
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
    use luned::api::{show_request, show_response};
    use luned::mux::own::frames::{Cursor, Ending, Glyph, Grapheme, Line, Shows};
    use luned::mux::own::OwnMade;
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

    fn api_key(id: u32) -> luned::api::TerminalKey {
        luned::api::TerminalKey::from(key(id))
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
        let tabs = vec![
            Tab { worktree: worktree(1), name: "one".to_owned() },
            Tab { worktree: worktree(2), name: "two".to_owned() },
        ];
        let project = ProjectId::of(2).unwrap();
        assert_eq!(colour_of(project), moonlight::MAUVE);
        layout.worktree_tabs = Some(WorktreeTabs { project, tabs });
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
        let cursor = layout.draw(&mut buffer, Rect::new(2, 0, 22, 6), None, None);
        assert_eq!(row(&buffer, 0, 2, 17), "\u{e0b6} one \u{e0b4}   two  ");
        assert_eq!((buffer[(3, 0)].fg, buffer[(3, 0)].bg), (moonlight::INK, moonlight::MAUVE));
        assert_eq!((buffer[(2, 0)].fg, buffer[(2, 0)].bg), (moonlight::MAUVE, Color::Reset));
        assert_eq!((buffer[(12, 0)].fg, buffer[(12, 0)].bg), (moonlight::MUTE, Color::Reset));
        assert_eq!(row(&buffer, 1, 2, 22), "╭ p1 ────╮╭ p2 ────╮");
        assert_eq!(row(&buffer, 2, 2, 12), "│abcdefgh│");
        assert_eq!(row(&buffer, 3, 12, 22), "│yz012345│");
        assert_eq!(row(&buffer, 4, 2, 22), "╰────────╯╰────────╯");
        assert_eq!((buffer[(2, 1)].fg, buffer[(12, 1)].fg), (moonlight::MUTE, moonlight::LIFTED));
        assert_eq!(row(&buffer, 5, 2, 24), " ".repeat(22));
        assert_eq!(cursor, Some(Position::new(5, 3)));
        let mut small = Buffer::empty(Rect::new(0, 0, 14, 3));
        assert_eq!(layout.draw(&mut small, Rect::new(0, 0, 14, 3), None, None), None);
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
        assert_eq!(layout.edged(worktree(1), Direction::Right), Some(Across::After));
        layout.go(worktree(2));
        assert_eq!(layout.edged(worktree(1), Direction::Right), None, "an edge of the one left");
        assert_eq!(layout.edged(worktree(2), Direction::Left), Some(Across::Before));
    }

    #[test]
    fn a_full_terminal_fills_the_view_and_a_lost_one_or_the_one_lune_runs_in_says_so() {
        let mut layout = split_layout();
        layout.laid_out(laid(halves(), 12, true));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), None, None);
        assert_eq!(row(&buffer, 1, 0, 20), "╭ p2 ──────────────╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        let mut here = Layout::new(worktree(1), Some(key(2)));
        let lost = vec![view(11, (0, 0, 10, 4), None), view(12, (10, 0, 10, 4), Some(2))];
        here.laid_out(laid(lost, 11, false));
        assert_eq!(here.wanted(), BTreeSet::new(), "lune never shows the terminal it runs in");
        here.draw(&mut buffer, Rect::new(0, 0, 20, 5), None, None);
        assert_eq!(row(&buffer, 2, 1, 9), "lost: lu");
        assert_eq!(row(&buffer, 2, 11, 19), "lune run");
    }

    #[test]
    fn tabs_draw_the_focused_view_whole_with_each_view_named_on_its_border() {
        use show_response::laid::Shown as Said;
        let mut layout = split_layout();
        let tab = |id, key| view(id, (0, 0, 20, 4), key);
        let tabs = |focused| {
            Laid::heard(show_response::Laid {
                worktree: 1,
                size: Some(show_request::Size { columns: 20, rows: 4 }),
                views: vec![tab(11, Some(1)), tab(12, Some(2)), tab(13, None)],
                shown: Some(Said::Tabbed(focused)),
            })
            .unwrap()
        };
        layout.laid_out(tabs(12));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), None, None);
        assert_eq!(row(&buffer, 1, 0, 20), "╭  p  ─ ● p ─  t  ─╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        assert_eq!((buffer[(8, 1)].symbol(), buffer[(8, 1)].fg), (VIEW_DOT, moonlight::MAUVE));
        assert_eq!(buffer[(10, 1)].fg, moonlight::TEXT);
        assert!(buffer[(10, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(3, 1)].fg, moonlight::MUTE, "a tab not focused is dim");
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        assert_eq!(layout.moused(down, spot(1, 2), 20, None), Moused::Focus(terminal(11)));
        assert_eq!(layout.moused(down, spot(1, 14), 20, None), Moused::Focus(terminal(13)));
        let on_the_focused = layout.moused(down, spot(1, 7), 20, None);
        assert_eq!(on_the_focused, Moused::Nothing, "the focused tab is there");
        assert!(!matches!(layout.moused(down, spot(2, 3), 20, None), Moused::Focus(_)));
    }

    #[test]
    fn tabs_share_the_border_and_the_focused_one_is_always_drawn() {
        use show_response::laid::Shown as Said;
        let mut layout = split_layout();
        let long = |title: &str| title.to_owned();
        layout.titles =
            [(terminal(11), long("a very long title")), (terminal(12), long("another long one"))]
                .into();
        let tab = |id, key| view(id, (0, 0, 20, 4), key);
        let tabs = Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size { columns: 20, rows: 4 }),
            views: vec![tab(11, Some(1)), tab(12, Some(2)), tab(13, None)],
            shown: Some(Said::Tabbed(13)),
        });
        layout.laid_out(tabs.unwrap());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), None, None);
        assert_eq!(row(&buffer, 1, 0, 20), "╭  a  ─  a  ─ ● t ─╮");
        let drawn = |columns| {
            let spans = layout.view_tabs(Rect::new(0, 0, columns, 4), None);
            spans.iter().map(|span| (span.id, span.column, span.columns)).collect::<Vec<_>>()
        };
        let (one, two, three) = (terminal(11), terminal(12), terminal(13));
        assert_eq!(drawn(7), [(one, 0, 1), (two, 2, 1), (three, 4, 1)], "a column each");
        assert_eq!(drawn(6), [(two, 0, 1), (three, 2, 1)], "the tabs before it go first");
        assert_eq!(drawn(3), [(three, 0, 1)], "the focused tab alone");
        assert!(drawn(2).is_empty(), "a border with no room draws no tab");
    }

    #[test]
    fn a_press_focuses_the_terminal_under_it_or_goes_to_its_worktree_or_reaches_the_program() {
        let mut layout = split_layout();
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left));
        let up = mouse(MouseEventKind::Up(MouseButton::Left));
        let at = |moused: Moused| match moused {
            Moused::Sent(key, said) => Some((key.id.get(), said.row, said.column)),
            Moused::Nothing | Moused::Go(_) | Moused::Focus(_) | Moused::Browser { .. } => None,
        };
        assert_eq!(layout.moused(down, spot(2, 14), 22, None), Moused::Focus(terminal(12)));
        assert_eq!(layout.moused(down, spot(0, 9), 22, None), Moused::Go(worktree(2)));
        assert_eq!(layout.moused(down, spot(0, 1), 22, None), Moused::Nothing);
        assert_eq!(layout.moused(down, spot(1, 0), 22, None), Moused::Nothing);
        assert_eq!(at(layout.moused(down, spot(2, 3), 22, None)), Some((1, 0, 2)));
        assert_eq!(at(layout.moused(drag, spot(0, 15), 22, None)), Some((1, 0, 7)));
        assert_eq!(at(layout.moused(up, spot(4, 15), 22, None)), Some((1, 1, 7)));
        assert_eq!(layout.moused(up, spot(2, 3), 22, None), Moused::Nothing);
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
        layout.draw(&mut buffer, Rect::new(0, 0, 20, 5), Some(spot(2, 2)), None);
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
        let elsewhere = "an input lune takes itself between the two starts over";
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
        let browser_config = embedded::Config {
            runtime: folder.clone(),
            terminal_socket: folder.join(luned::terminal_socket::SOCKET),
            cell: CellPixels { width: 10, height: 20 },
            colours: Colours::default(),
            supported: false,
        };
        let mut viewed = Viewed::start(
            1,
            folder,
            Machine::Local,
            worktree(1),
            size,
            greeting,
            None,
            browser_config,
            &events,
        );
        let one = || vec![view(11, (0, 0, 20, 4), Some(1))];
        assert!(matches!(viewed.heard(laid_in(1, Vec::new())), Heeded::Quiet), "first, it opens");
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Quiet));
        let emptied = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(emptied, Heeded::Emptied), "its last terminal closed, lune leaves it");
        viewed.go(&Machine::Local, worktree(2));
        let two = vec![view(21, (0, 0, 20, 4), Some(2))];
        assert!(matches!(viewed.heard(laid_in(2, two)), Heeded::Quiet));
        viewed.go(&Machine::Local, worktree(1));
        let back = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(back, Heeded::Quiet), "gone back to, it opens as it did first");
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Quiet));
        let again = viewed.heard(laid_in(1, Vec::new()));
        assert!(matches!(again, Heeded::Emptied), "emptied again, lune leaves it again");
    }

    fn pasted(text: &str) -> typed::Event {
        typed::Event::Paste(api::Paste { text: text.to_owned() })
    }

    fn found(terminal: u64, said: show_response::view::Found) -> show_response::View {
        let mut it = view(terminal, (0, 0, 20, 4), None);
        it.found = Some(said);
        it
    }

    fn laid_on(id: u64, focused: show_response::View) -> Laid {
        let Heard::Laid(laid) = laid_in(id, vec![focused]) else { unreachable!() };
        laid
    }

    fn sent(layout: &mut Layout, now: Instant) -> Option<(TerminalKey, VecDeque<typed::Event>)> {
        layout.take_unsent(Some(()), now).map(|((), key, events)| (key, events))
    }

    #[test]
    fn events_typed_before_a_terminal_wait_for_it_and_go_in_order_once_it_is_shown() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), now);
        assert_eq!(sent(&mut layout, now), None, "no layout yet");
        assert!(!layout.laid_out(laid_on(1, found(11, Said::Looking(())))));
        assert_eq!(sent(&mut layout, now), None, "its terminal is yet to be found");
        assert!(!layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))));
        assert_eq!(layout.take_unsent(None::<()>, now), None, "no viewer yet");
        let sent_now = sent(&mut layout, now);
        assert_eq!(sent_now, Some((key(1), VecDeque::from([pasted("a"), pasted("b")]))));
        assert_eq!(sent(&mut layout, now), Some((key(1), VecDeque::new())));
    }

    #[test]
    fn events_kept_are_dropped_for_a_terminal_lost_welcoming_or_lune_s_own() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let terminal = || view(11, (0, 0, 20, 4), Some(1));
        let nowhere = [found(11, Said::Lost(())), found(11, Said::Welcome(()))];
        for focused in nowhere {
            let mut layout = Layout::new(worktree(1), None);
            layout.keep_unsent(pasted("a"), now);
            assert!(!layout.laid_out(laid_on(1, focused)));
            assert_eq!(sent(&mut layout, now), None);
            assert!(!layout.laid_out(laid_on(1, terminal())));
            assert_eq!(sent(&mut layout, now), Some((key(1), VecDeque::new())));
        }
        let mut layout = Layout::new(worktree(1), Some(key(1)));
        layout.keep_unsent(pasted("a"), now);
        assert!(!layout.laid_out(laid_on(1, terminal())));
        assert_eq!(sent(&mut layout, now), None, "lune types not into itself");
        assert!(matches!(layout.unsent.held, Held::Nothing));
    }

    #[test]
    fn events_kept_for_a_terminal_never_reach_the_one_put_in_its_view_in_its_place() {
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))));
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(layout.take_unsent(None::<()>, now), None, "its viewer is lost");
        assert!(!layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(2)))));
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())), "remade");
    }

    #[test]
    fn events_kept_for_one_worktree_never_reach_another_gone_to_or_followed() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        layout.keep_unsent(pasted("a"), now);
        layout.go(worktree(2));
        assert!(!layout.laid_out(laid_on(2, view(21, (0, 0, 20, 4), Some(2)))));
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())), "gone to");
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid_on(1, found(11, Said::Looking(())))));
        layout.keep_unsent(pasted("a"), now);
        let followed = laid_on(2, view(21, (0, 0, 20, 4), Some(2)));
        assert!(layout.followed(&followed));
        assert!(!layout.laid_out(followed));
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())), "followed");
    }

    #[test]
    fn events_kept_for_one_view_never_reach_another_focused_in_its_place() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid_on(1, found(11, Said::Looking(())))));
        layout.keep_unsent(pasted("a"), now);
        assert!(!layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))));
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())));
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))));
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(layout.take_unsent(None::<()>, now), None, "no viewer yet");
        assert!(!layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))));
        layout.keep_unsent(pasted("b"), now);
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::from([pasted("b")]))));
    }

    #[test]
    fn events_kept_longer_than_the_most_time_are_dropped_with_those_after_them() {
        let now = Instant::now();
        let later = now + UNSENT_FOR;
        let mut layout = Layout::new(worktree(1), None);
        assert!(!layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))));
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(sent(&mut layout, later), Some((key(1), VecDeque::from([pasted("a")]))));
        let too_late = later + Duration::from_millis(1);
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), later);
        assert_eq!(sent(&mut layout, too_late), Some((key(1), VecDeque::new())));
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), too_late);
        layout.keep_unsent(pasted("c"), too_late);
        assert_eq!(sent(&mut layout, too_late), Some((key(1), VecDeque::new())), "no tail");
        layout.keep_unsent(pasted("d"), too_late);
        let taken = Some((key(1), VecDeque::from([pasted("d")])));
        assert_eq!(sent(&mut layout, too_late), taken, "a terminal took input, so on it goes");
    }

    #[test]
    fn events_past_the_most_kept_are_dropped_and_those_kept_stay_in_order() {
        let now = Instant::now();
        let aim = Aim { worktree: worktree(1), view: None, key: None };
        let mut unsent = Unsent::new(aim);
        for index in 0..=UNSENT_MAX {
            unsent.keep(aim, pasted(&index.to_string()), now);
        }
        assert!(matches!(&unsent.held, Held::Kept(kept) if kept.dropped));
        let kept: Vec<typed::Event> = unsent.take().into();
        let first = (0..UNSENT_MAX).map(|index| pasted(&index.to_string()));
        assert_eq!(kept, first.collect::<Vec<typed::Event>>());
        assert!(matches!(unsent.held, Held::Nothing), "a batch sent starts the count over");
    }

    #[test]
    fn browser_view_draws_fallback_and_handles_open_copy_and_close_when_unsupported() {
        use show_response::view::{Browser, Found as Said};
        let mut layout = Layout::new(worktree(1), None);
        let browser = show_response::View {
            id: 31,
            rect: Some(show_response::Rect { column: 0, row: 0, columns: 48, rows: 10 }),
            found: Some(Said::Browser(Browser {
                url: Some("https://example.com/pull/809".to_owned()),
            })),
        };
        let laid_browser = Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size { columns: 48, rows: 10 }),
            views: vec![browser],
            shown: Some(show_response::laid::Shown::Tiled(31)),
        })
        .unwrap();
        assert!(!layout.laid_out(laid_browser));
        let (_, url, _) = layout.focused_browser().unwrap();
        assert_eq!(url.map(BrowserUrl::as_str), Some("https://example.com/pull/809"));

        let area = Rect::new(0, 0, 48, 12);
        let mut buffer = Buffer::empty(area);
        let (events, _rx) = std::sync::mpsc::channel();
        let config = embedded::Config {
            runtime: PathBuf::from("/tmp"),
            terminal_socket: PathBuf::from("/tmp").join(luned::terminal_socket::SOCKET),
            cell: crate::answers::CellPixels { width: 10, height: 20 },
            colours: crate::answers::Colours::default(),
            supported: false,
        };
        let browsers = embedded::Browsers::new(1, config, events);
        layout.draw(&mut buffer, area, None, Some(&browsers));
        let rendered: String = (0..area.height)
            .map(|y| row(&buffer, y, 0, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("https://example.com/pull/809"), "{rendered}");
        assert!(rendered.contains("o  open in OS browser"), "{rendered}");
        assert!(rendered.contains("y  copy URL"), "{rendered}");
    }
}
