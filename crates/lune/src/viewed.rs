use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::CursorShape;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{
    Arranged, BrowserUrl, LAYOUT_VIEWS_MAX, Opens, ProjectId, Size, ViewId, WorktreeId,
};
use luned::folder;
use luned::log;
use luned::mux::own::api::{self, typed};
use luned::mux::own::frames::{self, Frame};
use luned::mux::own::viewer::{self, Greeting, Reach, Seen, Viewer};
use luned::{
    AskNumber, Beside, Direction, Echoed, FocusAsked, Found, Growth, Heard, Laid, LayoutAsked,
    Rect as Placed, Show, Shows, View, Where,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::answers::{CellPixels, Colours};
use crate::attached::{
    Applied, Clean, Pointer, Screen, Selection, Spot, TAB_BAR_ROWS, TAB_PAD, TabSpan, below_bar,
    border, draw_screen, draw_tab_spans, key_said, link, mouse_said, on_views, screen_rect,
    selects, span_under, spot_in, tab_columns_max, tab_spans, tab_spans_capped, text, within,
};
use crate::copied::Excerpt;
use crate::desktop;
use crate::embedded::{self, Browsers, Desired, Outcome, PointerShape, Shown};
use crate::host::Event;
use crate::machines::Machine;
use crate::moonlight;
use crate::review::{self, KittyTarget, LoadedDiff, ReviewAction, ReviewPane, RibbonOverlay};
use crate::sidebar::colour_of;

const AGAIN: Duration = Duration::from_secs(1);

const VIEW_DOT: &str = "●";

const LOST: &str = "lost: lune-mux no longer holds it";

const LOOKING: &str = "looking for it in lune-mux";

const HERE: &str = "lune runs here";

const UNTITLED: &str = "terminal";

const WELCOME_TAB: &str = "welcome";

const BROWSER_TAB: &str = "browser";

const WELCOME: [&str; 4] = [
    "Welcome",
    "",
    "This view runs no terminal.",
    "d d  close it   Ctrl hjkl  another view",
];

const BROWSER_UNSUPPORTED_TERMINAL: &str = "This terminal does not support Kitty graphics.";

const BROWSER_UNSUPPORTED_SSH: &str = "Inline browser graphics are unavailable over SSH.";

const BROWSER_LIMIT_REACHED: &str =
    "At the limit of inline browsers; close another to render this one.";

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
    let rows = NonZeroU16::new(rows.get().saturating_sub(TAB_BAR_ROWS).max(1))
        .expect("a layout has at least 1 row");
    Size::within(columns, rows)
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
    Browser {
        view: ViewId,
        mouse: MouseEvent,
        column: u16,
        row: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Across {
    Before,
    After,
}

pub enum Heeded {
    Quiet,
    Changed,
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
    awaiting: Option<WaitingFor>,
    screens: BTreeMap<TerminalKey, Screen>,
    reviews: BTreeMap<ViewId, ReviewPane>,
    pointer: Pointer,
    worktree_tabs: Option<WorktreeTabs>,
    titles: BTreeMap<ViewId, String>,
    closing: Closing,
    unsent: Unsent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitingFor {
    Focus(AskNumber),
    Step { ask: AskNumber, step: NonZeroU64 },
}

impl WaitingFor {
    fn ask(self) -> AskNumber {
        match self {
            WaitingFor::Focus(ask) | WaitingFor::Step { ask, .. } => ask,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeHeard {
    Ignored,
    Stayed,
    Crosses(Across),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaidOut {
    Ignored,
    Shown,
    Emptied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AimFocus {
    Awaiting(AskNumber),
    Unlaid,
    View(ViewId),
    Terminal(ViewId, TerminalKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Aim {
    worktree: WorktreeId,
    focus: AimFocus,
}

impl Aim {
    fn reaches(self, now: Aim) -> bool {
        if self.worktree != now.worktree {
            return false;
        }
        match (self.focus, now.focus) {
            (AimFocus::Awaiting(asked), AimFocus::Awaiting(current)) => asked == current,
            (AimFocus::Unlaid, AimFocus::Awaiting(_))
            | (
                AimFocus::Awaiting(_) | AimFocus::Unlaid,
                AimFocus::Unlaid | AimFocus::View(_) | AimFocus::Terminal(..),
            ) => true,
            (AimFocus::View(before), AimFocus::View(now) | AimFocus::Terminal(now, _)) => {
                before == now
            }
            (AimFocus::Terminal(before, had), AimFocus::Terminal(now, has)) => {
                before == now && had == has
            }
            (
                AimFocus::View(_) | AimFocus::Terminal(..),
                AimFocus::Awaiting(_) | AimFocus::Unlaid,
            )
            | (AimFocus::Terminal(..), AimFocus::View(_)) => false,
        }
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
        Unsent {
            aim,
            held: Held::Nothing,
        }
    }

    fn aim_at(&mut self, aim: Aim, now: Instant) {
        let moved = !self.aim.reaches(aim);
        self.aim = aim;
        if moved {
            if let Held::Kept(kept) = std::mem::replace(&mut self.held, Held::Nothing) {
                let count = kept.events.len();
                let dropped = format!("lune drops {count} events kept for a terminal yet to come");
                log::say(&format!(
                    "{dropped}: typed for a terminal no longer focused"
                ));
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
        log::say(&format!(
            "{dropped}: typed over {UNSENT_FOR:?} ago, {after}"
        ));
    }

    fn keep(&mut self, aim: Aim, event: typed::Event, now: Instant) {
        self.aim_at(aim, now);
        let kept = match &mut self.held {
            Held::Nothing => {
                let events = VecDeque::from([event]);
                self.held = Held::Kept(Kept {
                    since: now,
                    events,
                    dropped: false,
                });
                return;
            }
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

    fn drop_kept(&mut self, aim: Aim, why: &str, now: Instant) {
        if let Held::Kept(kept) = std::mem::replace(&mut self.held, Held::Nothing) {
            let count = kept.events.len();
            let dropped = format!("lune drops {count} events kept for a terminal yet to come");
            log::say(&format!("{dropped}: {why}"));
        }
        self.aim_at(aim, now);
        assert_eq!(self.aim, aim, "dropped wait aims at the current layout");
        assert!(
            matches!(self.held, Held::Nothing),
            "dropped wait keeps no events"
        );
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

fn awaited_reached(awaiting: Option<WaitingFor>, heard: Echoed) -> bool {
    match (awaiting, heard) {
        (None, _) => false,
        (Some(_), Echoed::Unnumbered) => true,
        (Some(waiting), Echoed::Through(heard)) => heard >= waiting.ask(),
    }
}

impl Layout {
    fn new(worktree: WorktreeId, runs_in: Option<TerminalKey>) -> Layout {
        Layout {
            worktree,
            runs_in,
            laid: None,
            awaiting: None,
            screens: BTreeMap::new(),
            reviews: BTreeMap::new(),
            pointer: Pointer::Free,
            worktree_tabs: None,
            titles: BTreeMap::new(),
            closing: Closing::Unarmed,
            unsent: Unsent::new(Aim {
                worktree,
                focus: AimFocus::Unlaid,
            }),
        }
    }

    fn laid(&self) -> Option<&Laid> {
        self.laid.as_ref()
    }

    fn focused(&self) -> Option<ViewId> {
        self.laid()?.focused().map(|view| view.id)
    }

    fn arranged(&self) -> Option<(Arranged, Option<luned::api::Version>)> {
        let laid = self.laid()?;
        let arranged = match laid.shows()? {
            Shows::Tabs => Arranged::Tabs,
            Shows::Tiled | Shows::Full => Arranged::Panes,
        };
        Some((arranged, laid.version()))
    }

    fn welcomes(&self) -> bool {
        self.awaiting.is_none()
            && self
                .laid()
                .and_then(Laid::focused)
                .is_some_and(|view| view.found == Found::Welcome)
    }

    fn browses(&self) -> bool {
        self.awaiting.is_none()
            && self
                .laid()
                .and_then(Laid::focused)
                .is_some_and(|view| matches!(view.found, Found::Browser { .. }))
    }

    fn focused_browser(&self) -> Option<(ViewId, Option<&BrowserUrl>, Rect)> {
        if self.awaiting.is_some() {
            return None;
        }
        let (view, placed) = self.laid()?.focused_at()?;
        let Found::Browser { url } = &view.found else {
            return None;
        };
        let inner = screen_rect(view_rect(Rect::default(), placed));
        Some((view.id, url.as_ref(), inner))
    }

    fn reviews(&self) -> bool {
        self.awaiting.is_none()
            && self
                .laid()
                .and_then(Laid::focused)
                .is_some_and(|view| view.found == Found::Review)
    }

    fn has_review(&self) -> bool {
        !self.reviews.is_empty()
    }

    fn review_needs_load(&self) -> bool {
        self.reviews.values().any(ReviewPane::is_loading)
    }

    fn closes(&mut self, key: &KeyEvent, input: u64) -> Option<ViewId> {
        assert!(
            self.welcomes() || self.browses() || self.reviews(),
            "only a welcome, browser, or review view is closed by its keys"
        );
        let focused = self
            .focused()
            .expect("a welcome, browser, or review view is focused");
        let closing = std::mem::replace(&mut self.closing, Closing::Unarmed);
        let pressed = key.kind == KeyEventKind::Press && key.modifiers.is_empty();
        if !pressed || key.code != KeyCode::Char(CLOSE) {
            return None;
        }
        let next = |armed: u64| armed.checked_add(1) == Some(input);
        let twice =
            matches!(closing, Closing::Armed { view, input } if view == focused && next(input));
        if twice {
            self.closing = Closing::Closed;
            return Some(focused);
        }
        self.closing = Closing::Armed {
            view: focused,
            input,
        };
        None
    }

    fn disarm_close(&mut self) {
        if matches!(self.closing, Closing::Armed { .. }) {
            self.closing = Closing::Unarmed;
        }
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
        if self.awaiting.is_some() {
            return Typing::Later;
        }
        let Some(focused) = self.laid().and_then(Laid::focused) else {
            return Typing::Later;
        };
        match &focused.found {
            Found::Key(key) if Some(*key) == self.runs_in => Typing::Never,
            Found::Key(key) => Typing::Into(*key),
            Found::Looking => Typing::Later,
            Found::Lost | Found::Welcome | Found::Browser { .. } | Found::Review => Typing::Never,
        }
    }

    fn aim(&self) -> Aim {
        let focus = match self.awaiting {
            Some(waiting) => AimFocus::Awaiting(waiting.ask()),
            None => match self.laid().and_then(Laid::focused) {
                None => AimFocus::Unlaid,
                Some(view) => match view.key() {
                    Some(key) => AimFocus::Terminal(view.id, key),
                    None => AimFocus::View(view.id),
                },
            },
        };
        Aim {
            worktree: self.worktree,
            focus,
        }
    }

    fn await_focus(&mut self, waiting: WaitingFor, now: Instant) {
        let asked = waiting.ask();
        assert!(
            asked > AskNumber::HELLO,
            "awaited focus asks come after hello"
        );
        if let Some(before) = self.awaiting {
            assert!(
                asked > before.ask(),
                "awaited focus asks climb strictly on one stream"
            );
        }
        self.awaiting = Some(waiting);
        let aim = self.aim();
        self.unsent.aim_at(aim, now);
        assert_eq!(
            self.unsent.aim, aim,
            "unsent input aims at the awaited focus"
        );
    }

    fn heard_laid(&mut self, heard: Echoed, now: Instant) {
        if awaited_reached(self.awaiting, heard) {
            self.awaiting = None;
            let aim = self.aim();
            self.unsent.aim_at(aim, now);
            assert!(self.awaiting.is_none(), "reached layout clears focus wait");
            assert_eq!(self.unsent.aim, aim, "reached layout syncs unsent aim");
        }
    }

    fn heard_refused(&mut self, heard: Echoed, now: Instant) {
        if awaited_reached(self.awaiting, heard) {
            self.awaiting = None;
            self.drop_awaiting("luned refused the focus change", now);
        }
    }

    fn heard_edged(
        &mut self,
        worktree: WorktreeId,
        direction: Direction,
        step: NonZeroU64,
        heard: Echoed,
        now: Instant,
    ) -> EdgeHeard {
        if worktree != self.worktree {
            return EdgeHeard::Ignored;
        }
        let Some(waiting @ WaitingFor::Step { step: awaited, .. }) = self.awaiting else {
            return EdgeHeard::Ignored;
        };
        if step != awaited || !awaited_reached(Some(waiting), heard) {
            return EdgeHeard::Ignored;
        }
        if let Some(across) = self.edged(worktree, direction) {
            return EdgeHeard::Crosses(across);
        }
        self.stayed_at_edge(now);
        EdgeHeard::Stayed
    }

    fn stayed_at_edge(&mut self, now: Instant) {
        if matches!(self.awaiting, Some(WaitingFor::Step { .. })) {
            self.awaiting = None;
            let aim = self.aim();
            self.unsent.aim_at(aim, now);
            assert_eq!(self.unsent.aim, aim, "staying edge syncs unsent aim");
        }
    }

    fn crossed_edge(&mut self, now: Instant) {
        if matches!(self.awaiting, Some(WaitingFor::Step { .. })) {
            self.awaiting = None;
            self.drop_awaiting("the focus step crossed out of the layout", now);
        }
    }

    fn forget_step(&mut self, now: Instant) {
        if matches!(self.awaiting, Some(WaitingFor::Step { .. })) {
            self.awaiting = None;
            self.drop_awaiting("focus left the terminal before the step answered", now);
        }
    }

    fn stream_lost(&mut self, now: Instant) {
        if self.awaiting.take().is_some() {
            self.drop_awaiting("luned's layout stream was lost", now);
        }
    }

    fn drop_awaiting(&mut self, why: &str, now: Instant) {
        assert!(
            self.awaiting.is_none(),
            "only a finished wait drops kept events"
        );
        let aim = self.aim();
        self.unsent.drop_kept(aim, why, now);
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
            }
        };
        assert_ne!(
            Some(key),
            self.runs_in,
            "lune types nothing into its own terminal"
        );
        let viewer = viewer?;
        Some((viewer, key, self.unsent.take()))
    }

    fn go(&mut self, worktree: WorktreeId, now: Instant) {
        self.worktree = worktree;
        self.laid = None;
        self.awaiting = None;
        self.screens.clear();
        self.reviews.clear();
        self.pointer = Pointer::Free;
        self.unsent
            .drop_kept(self.aim(), "lune left the worktree it typed into", now);
    }

    fn left(&mut self, worktree: WorktreeId, now: Instant) -> bool {
        if worktree != self.worktree {
            return false;
        }
        self.go(worktree, now);
        assert!(
            self.wanted().is_empty(),
            "a worktree gone shows no terminal"
        );
        true
    }

    fn followed(&mut self, laid: &Laid, now: Instant) -> bool {
        let arrived = self.laid.is_some();
        let follows = arrived && laid.worktree() != self.worktree;
        if follows {
            self.worktree = laid.worktree();
            self.laid = None;
            self.pointer = Pointer::Free;
            self.unsent.drop_kept(
                self.aim(),
                "lune followed a terminal to another worktree",
                now,
            );
        }
        follows
    }

    fn laid_out(&mut self, laid: Laid) -> LaidOut {
        if laid.worktree() != self.worktree {
            return LaidOut::Ignored;
        }
        if self.focused() != laid.focused().map(|view| view.id) {
            self.pointer = Pointer::Free;
            if matches!(self.closing, Closing::Armed { .. }) {
                self.closing = Closing::Unarmed;
            }
        }
        let empty = laid.is_empty();
        let review_ids: BTreeSet<ViewId> = laid
            .views()
            .filter(|view| view.found == Found::Review)
            .map(|view| view.id)
            .collect();
        self.reviews.retain(|id, _| review_ids.contains(id));
        for id in review_ids {
            self.reviews.entry(id).or_insert_with(ReviewPane::loading);
        }
        self.laid = Some(laid);
        let wanted = self.wanted();
        self.screens.retain(|key, _| wanted.contains(key));
        if empty {
            LaidOut::Emptied
        } else {
            LaidOut::Shown
        }
    }

    fn wanted(&self) -> BTreeSet<TerminalKey> {
        let mut keys = self.laid().map_or_default(Laid::keys);
        if let Some(runs_in) = self.runs_in {
            keys.remove(&runs_in);
        }
        keys
    }

    fn framed(&mut self, frame: Frame) -> Result<bool, String> {
        let key = frame.terminal();
        if !self.wanted().contains(&key) {
            return Ok(false);
        }
        let applied = Screen::apply(&mut self.screens, frame)?;
        if applied == Applied::Resized && self.focused_key() == Some(key) {
            self.pointer = self.pointer.unselected();
        }
        Ok(applied.changed())
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
        let focused = laid
            .focused()
            .expect("a layout shown as tabs focuses one")
            .id;
        let named: Vec<(ViewId, String)> = laid
            .in_order()
            .map(|view| (view.id, self.view_title(view, browsers)))
            .collect();
        let named_refs: Vec<(ViewId, &str)> = named
            .iter()
            .map(|(id, name)| (*id, name.as_str()))
            .collect();
        let from_each = (0..named_refs.len()).map(|hidden| {
            let shown = &named_refs[hidden..];
            tab_spans_capped(shown.iter().copied(), width, tab_columns_max(shown, width))
        });
        let mut drawn = from_each;
        let drawn = drawn.find(|spans| spans.iter().any(|span| span.id == focused));
        drawn.expect("the focused tab is drawn on a border of a column or more")
    }

    fn view_title(&self, view: &View, browsers: Option<&Browsers>) -> String {
        match &view.found {
            Found::Browser { url } => browsers
                .and_then(|browsers| browsers.title(view.id))
                .or_else(|| url.as_ref().map(BrowserUrl::short))
                .unwrap_or(BROWSER_TAB)
                .to_owned(),
            Found::Welcome => WELCOME_TAB.to_owned(),
            Found::Review => self
                .titles
                .get(&view.id)
                .filter(|it| !it.is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    self.reviews
                        .get(&view.id)
                        .map_or_else(|| review::TAB.to_owned(), ReviewPane::tab_label)
                }),
            Found::Key(_) | Found::Lost | Found::Looking => self
                .titles
                .get(&view.id)
                .map(String::as_str)
                .filter(|it| !it.is_empty())
                .unwrap_or(UNTITLED)
                .to_owned(),
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
        if let Some((view, placed)) = self.laid().and_then(Laid::focused_at)
            && view.found == Found::Review
        {
            let inner = screen_rect(view_rect(Rect::default(), placed));
            if inner.contains(spot) {
                let local = Position::new(spot.x - inner.x, spot.y - inner.y);
                let id = view.id;
                if let Some(pane) = self.reviews.get_mut(&id) {
                    pane.mouse(mouse, local);
                }
            }
            return Moused::Nothing;
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
                    self.pointer = if inside {
                        Pointer::Sending
                    } else {
                        Pointer::Free
                    };
                    inside
                }
                MouseEventKind::Drag(_) => sending,
                MouseEventKind::Up(_) => {
                    if sending {
                        self.pointer = Pointer::Free;
                    }
                    sending
                }
                MouseEventKind::Moved
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight => inside,
            };
            if sends {
                let spot = spot_in(inner, at);
                return Moused::Browser {
                    view,
                    mouse,
                    column: spot.column,
                    row: spot.row,
                };
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
                self.pointer = if inside {
                    Pointer::Sending
                } else {
                    Pointer::Free
                };
                inside
            }
            MouseEventKind::Drag(_) => sending,
            MouseEventKind::Up(_) => {
                if sending {
                    self.pointer = Pointer::Free;
                }
                sending
            }
            MouseEventKind::Moved
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => inside,
        };
        let said = sends
            .then(|| mouse_said(mouse, spot_in(inner, at)))
            .flatten();
        said.map_or(Moused::Nothing, |said| Moused::Sent(key, said))
    }

    fn selects(&self, mouse: MouseEvent, at: Spot) -> bool {
        let Some((inner, _, screen)) = self.focused_screen() else {
            return false;
        };
        let inside = on_views(at).is_some_and(|at| inner.contains(at));
        inside && selects(screen.tracks_mouse(), mouse)
    }

    fn select_from(&mut self, at: Spot) {
        self.pointer = Pointer::Free;
        let (Some((inner, _, screen)), Some(at)) = (self.focused_screen(), on_views(at)) else {
            return;
        };
        let at = spot_in(inner, at);
        let held = within(at, screen.size());
        self.pointer = if held == at {
            Pointer::Pressed(at)
        } else {
            Pointer::Selecting(Selection {
                from: held,
                to: held,
            })
        };
    }

    fn select_to(&mut self, at: Spot) {
        let Some((inner, _, screen)) = self.focused_screen() else {
            return;
        };
        let at = within(spot_in(inner, below_bar(at)), screen.size());
        self.pointer = match self.pointer {
            Pointer::Pressed(from) if from != at => Pointer::Selecting(Selection { from, to: at }),
            Pointer::Selecting(selection) => Pointer::Selecting(Selection {
                to: at,
                ..selection
            }),
            Pointer::Free | Pointer::Sending | Pointer::Pressed(_) | Pointer::Shown(_) => {
                return;
            }
        };
    }

    fn selected(&mut self) -> Option<Excerpt> {
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let chosen = match self.pointer {
            Pointer::Pressed(at) => screen.and_then(|screen| link(screen, at)),
            Pointer::Selecting(selection) if selection.from != selection.to => {
                screen.and_then(|screen| Some((selection, Excerpt::of(text(screen, selection))?)))
            }
            Pointer::Selecting(_) => None,
            Pointer::Free | Pointer::Sending | Pointer::Shown(_) => return None,
        };
        self.pointer = chosen
            .as_ref()
            .map_or(Pointer::Free, |(lit, _)| Pointer::Shown(*lit));
        chosen.map(|(_, text)| text)
    }

    fn pointed(&self, at: Spot) -> Option<Selection> {
        let (inner, _, screen) = self.focused_screen()?;
        let at = on_views(at).filter(|at| inner.contains(*at))?;
        if screen.tracks_mouse() {
            return None;
        }
        let at = spot_in(inner, at);
        if within(at, screen.size()) != at {
            return None;
        }
        link(screen, at).map(|(span, _)| span)
    }

    fn draw(
        &mut self,
        clean: Clean<'_>,
        hover: Option<Spot>,
        browsers: Option<&Browsers>,
        kitty_cell_px: Option<(NonZeroU16, NonZeroU16)>,
    ) -> (Option<Position>, Vec<RibbonOverlay>) {
        let (buffer, area) = clean.into_buffer();
        if area.is_empty() {
            return (None, Vec::new());
        }
        let bar = Rect {
            height: area.height.min(TAB_BAR_ROWS),
            ..area
        };
        let views = Rect {
            y: area.y + bar.height,
            height: area.height - bar.height,
            ..area
        };
        let spans = tab_spans(self.named_tabs(), bar.width);
        let worktree_tabs = self.worktree_tabs.as_ref();
        let colour = worktree_tabs.map_or(moonlight::GREEN, |it| colour_of(it.project));
        draw_tab_spans(buffer, bar, &spans, self.worktree, colour);
        let focused = self.focused();
        let selection = self.pointer.selection();
        let pointed = hover.and_then(|at| self.pointed(at));
        let drawn_views: Vec<(ViewId, Found, Placed)> = self
            .drawn()
            .into_iter()
            .map(|(view, placed)| (view.id, view.found.clone(), placed))
            .collect();
        assert!(
            drawn_views.len() <= LAYOUT_VIEWS_MAX,
            "a layout draws at most {LAYOUT_VIEWS_MAX} views"
        );
        let mut cursor = None;
        let mut overlays = Vec::new();
        let mut review_slot = 0u32;
        for (view_id, view_found, placed) in drawn_views {
            let mine = Some(view_id) == focused;
            let outer = view_rect(views, placed);
            let edge = if mine {
                moonlight::MUTE
            } else {
                moonlight::LIFTED
            };
            let named = if mine {
                Style::new()
                    .fg(moonlight::TEXT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(moonlight::MUTE)
            };
            let tabs = self.view_tabs(outer, browsers);
            let titled = match &view_found {
                Found::Browser { url } => browsers
                    .and_then(|browsers| browsers.title(view_id))
                    .or_else(|| url.as_ref().map(BrowserUrl::short))
                    .unwrap_or(BROWSER_TAB)
                    .to_owned(),
                Found::Review => self
                    .titles
                    .get(&view_id)
                    .filter(|t| !t.is_empty())
                    .cloned()
                    .unwrap_or_else(|| {
                        self.reviews
                            .get(&view_id)
                            .map_or_else(|| review::TAB.to_owned(), ReviewPane::tab_label)
                    }),
                _ => self.titles.get(&view_id).cloned().unwrap_or_default(),
            };
            let title = if tabs.is_empty() { titled.as_str() } else { "" };
            border(buffer, views, outer, title, Style::new().fg(edge), named);
            draw_view_tabs(buffer, views, outer, &tabs, view_id, colour);
            let seen = screen_rect(outer).intersection(views);
            if seen.is_empty() {
                continue;
            }
            let said = match &view_found {
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
                }
                Found::Lost => LOST,
                Found::Looking => LOOKING,
                Found::Welcome => {
                    let style = Style::new().fg(if mine { colour } else { moonlight::MUTE });
                    for (line, y) in WELCOME.iter().zip(seen.top()..seen.bottom()) {
                        buffer.set_stringn(seen.x, y, line, usize::from(seen.width), style);
                    }
                    continue;
                }
                Found::Browser { url } => {
                    draw_browser(buffer, seen, view_id, url.as_ref(), mine, colour, browsers);
                    continue;
                }
                Found::Review => {
                    let kitty = kitty_cell_px.map(|cell_px| {
                        let image_id = review::KITTY_BASE_ID
                            .checked_add(review_slot)
                            .unwrap_or(NonZeroU32::MAX);
                        review_slot = review_slot.saturating_add(1);
                        KittyTarget { cell_px, image_id }
                    });
                    if let Some(pane) = self.reviews.get_mut(&view_id)
                        && let Some(overlay) = pane.draw(buffer, seen, mine, kitty)
                    {
                        overlays.push(overlay);
                    }
                    continue;
                }
            };
            let style = Style::new().fg(moonlight::MUTE);
            buffer.set_stringn(seen.x, seen.y, said, usize::from(seen.width), style);
        }
        if let Some(at) = cursor {
            assert!(
                views.contains(at),
                "a drawn cursor lies inside the layout's views"
            );
        }
        (cursor, overlays)
    }

    fn cursor_style(&self) -> (CursorShape, bool) {
        let screen = self.focused_screen().map(|(_, _, screen)| screen);
        let Some(cursor) = screen.and_then(Screen::cursor) else {
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
    let start = outer
        .x
        .checked_add(1)
        .expect("a view's border is a terminal column");
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
            let named = Style::new()
                .fg(moonlight::TEXT)
                .add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::raw(edge),
                Span::styled(VIEW_DOT, Style::new().fg(colour)),
                Span::raw(" "),
                Span::styled(name, named),
                Span::raw(rest(&lead)),
            ])
        } else {
            let pad = " ".repeat(usize::from(TAB_PAD));
            Line::styled(
                format!("{pad}{name}{}", rest(&pad)),
                Style::new().fg(moonlight::MUTE),
            )
        };
        if columns >= 2 * usize::from(TAB_PAD) {
            assert_eq!(
                said.width(),
                columns,
                "a tab paints the columns it was given"
            );
        }
        buffer.set_line(x, outer.y, &said, lit.width);
    }
}

fn paint_lines(buffer: &mut Buffer, seen: Rect, lines: &[(&str, Style)]) {
    assert!(
        (1..=8).contains(&lines.len()),
        "browser status panel paints 1 to 8 lines"
    );
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
                paint_lines(
                    buffer,
                    seen,
                    &[
                        ("Starting browser…", accent),
                        ("", dim),
                        (url_line, text_style),
                    ],
                );
                return;
            }
            Shown::AtLimit => {
                paint_lines(
                    buffer,
                    seen,
                    &[
                        ("Browser", accent),
                        ("", dim),
                        (url_line, text_style),
                        ("", dim),
                        (BROWSER_LIMIT_REACHED, dim),
                        ("", dim),
                        (BROWSER_FALLBACK_HELP, dim),
                    ],
                );
                return;
            }
            Shown::Failed(why) => {
                paint_lines(
                    buffer,
                    seen,
                    &[
                        ("Browser", accent),
                        ("", dim),
                        (url_line, text_style),
                        ("", dim),
                        (why, dim),
                        ("", dim),
                        (BROWSER_RETRY_HELP, dim),
                    ],
                );
                return;
            }
        }
    }
    let why = if desktop::elsewhere() {
        BROWSER_UNSUPPORTED_SSH
    } else {
        BROWSER_UNSUPPORTED_TERMINAL
    };
    paint_lines(
        buffer,
        seen,
        &[
            ("Browser", accent),
            ("", dim),
            (url_line, text_style),
            ("", dim),
            (why, dim),
            ("", dim),
            (BROWSER_FALLBACK_HELP, dim),
        ],
    );
}

fn view_rect(views: Rect, rect: Placed) -> Rect {
    let column = views
        .x
        .checked_add(rect.column)
        .expect("a view's column is a terminal column");
    let row = views
        .y
        .checked_add(rect.row)
        .expect("a view's row is a terminal row");
    Rect::new(column, row, rect.columns, rect.rows)
}

struct Watcher {
    number: u64,
    viewer: Viewer,
    showing: BTreeSet<TerminalKey>,
}

fn where_on(machine: &Machine, runs_in_here: Option<TerminalKey>) -> Where {
    match machine {
        Machine::Local => Where::Here {
            runs_in: runs_in_here,
        },
        Machine::Remote(destination) => Where::On(destination.clone()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreeOpen {
    Requested,
    Ready,
}

enum ShowStream {
    Disconnected,
    Connecting(Show),
    Live(Show),
}

impl ShowStream {
    fn is_disconnected(&self) -> bool {
        matches!(self, ShowStream::Disconnected)
    }

    fn is_live(&self) -> bool {
        matches!(self, ShowStream::Live(_))
    }

    fn mark_live(&mut self) {
        let current = std::mem::replace(self, ShowStream::Disconnected);
        *self = match current {
            ShowStream::Disconnected => ShowStream::Disconnected,
            ShowStream::Connecting(show) | ShowStream::Live(show) => ShowStream::Live(show),
        };
    }

    fn active(&self) -> Option<&Show> {
        match self {
            ShowStream::Disconnected => None,
            ShowStream::Connecting(show) | ShowStream::Live(show) => Some(show),
        }
    }

    fn active_mut(&mut self) -> Option<&mut Show> {
        match self {
            ShowStream::Disconnected => None,
            ShowStream::Connecting(show) | ShowStream::Live(show) => Some(show),
        }
    }
}

pub struct Viewed {
    number: u64,
    folder: PathBuf,
    machine: Machine,
    show: ShowStream,
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
    opened: BTreeMap<WorktreeId, WorktreeOpen>,
    steps: u64,
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
            show: ShowStream::Disconnected,
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
            opened: BTreeMap::new(),
            steps: 0,
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
        let show = luned::show(opened, self.at(), worktree, self.size, again, told);
        self.show = ShowStream::Connecting(show);
        if self.machine == Machine::Local {
            for (view, tty) in self.browsers.active_ttys() {
                self.ask(LayoutAsked::BrowserTty {
                    view,
                    tty: Some(tty),
                });
            }
        }
    }

    fn flush_browser_ttys(&mut self) {
        let changes = self.browsers.drain_tty_changes();
        if self.machine != Machine::Local {
            return;
        }
        for (view, tty) in changes {
            self.ask(LayoutAsked::BrowserTty { view, tty });
        }
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

    #[must_use]
    pub fn live(&self) -> bool {
        self.show.is_live()
    }

    #[must_use]
    pub fn arranged(&self) -> Option<(Arranged, Option<luned::api::Version>)> {
        self.show
            .is_live()
            .then(|| self.layout.arranged())
            .flatten()
    }

    pub fn welcomes(&self) -> bool {
        self.layout.welcomes()
    }

    pub fn browses(&self) -> bool {
        self.layout.browses()
    }

    pub fn browser(&mut self, view: ViewId, heard: embedded::Heard) -> Heeded {
        let failed = matches!(heard, embedded::Heard::Exited(embedded::Exit::Failed(_)));
        let title_before = self.browsers.title(view).map(str::to_owned);
        let pointer_before = self.browsers.pointer(view).cloned();
        let outcome = self.browsers.heard(view, heard);
        self.flush_browser_ttys();
        match outcome {
            Some(Outcome::Copy(text)) => Heeded::Copy(text),
            Some(Outcome::Navigated { view, url }) => {
                self.ask(LayoutAsked::Navigate { view, url });
                Heeded::Quiet
            }
            Some(Outcome::Close(view)) => {
                self.ask_focus(FocusAsked::Close(view));
                Heeded::Changed
            }
            None => {
                let changed = failed
                    || self.browsers.has_pending_graphics()
                    || self.browsers.title(view) != title_before.as_deref()
                    || self.browsers.pointer(view) != pointer_before.as_ref();
                if changed {
                    Heeded::Changed
                } else {
                    Heeded::Quiet
                }
            }
        }
    }

    pub fn colours(&mut self, colours: Colours) {
        self.browsers.colours(colours);
        self.flush_browser_ttys();
        assert_eq!(self.browsers.config().colours, colours);
    }

    pub fn cell_resized(&mut self, cell: CellPixels) {
        self.browsers.cell_resized(cell);
        self.flush_browser_ttys();
        assert_eq!(self.browsers.config().cell, cell);
    }

    #[must_use]
    pub fn has_pending_graphics(&self) -> bool {
        self.browsers.has_pending_graphics()
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
            self.flush_browser_ttys();
            return;
        };
        let drawn: BTreeMap<ViewId, Placed> = laid
            .drawn()
            .into_iter()
            .map(|(view, placed)| (view.id, placed))
            .collect();
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
                Some(Desired {
                    view: view.id,
                    url: url.as_ref(),
                    inner,
                    visible,
                })
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
        self.flush_browser_ttys();
    }

    pub fn reviews(&self) -> bool {
        self.layout.reviews()
    }

    pub fn has_review(&self) -> bool {
        self.layout.has_review()
    }

    pub fn review_needs_load(&self) -> bool {
        self.layout.review_needs_load()
    }

    fn lose_stream(&mut self, now: Instant) {
        self.show = ShowStream::Disconnected;
        self.layout.stream_lost(now);
    }

    pub fn heard(&mut self, event: Heard) -> Heeded {
        match event {
            Heard::Laid { laid, heard } => {
                let now = Instant::now();
                self.laid_in = Some(laid.worktree());
                let followed = self.layout.followed(&laid, now);
                if followed {
                    self.opened
                        .insert(self.layout.worktree, WorktreeOpen::Ready);
                }
                let was_live = self.show.is_live();
                let laid_changed = laid.worktree() == self.layout.worktree
                    && self.layout.laid.as_ref() != Some(&laid);
                let mut emptied = false;
                match self.layout.laid_out(laid) {
                    LaidOut::Ignored => {}
                    LaidOut::Shown => {
                        self.show.mark_live();
                        if self.opened.get(&self.layout.worktree) == Some(&WorktreeOpen::Requested)
                        {
                            self.opened
                                .insert(self.layout.worktree, WorktreeOpen::Ready);
                        }
                        self.layout.heard_laid(heard, now);
                    }
                    LaidOut::Emptied => {
                        self.show.mark_live();
                        self.layout.heard_laid(heard, now);
                        match self.opened.get(&self.layout.worktree) {
                            None => {
                                self.opened
                                    .insert(self.layout.worktree, WorktreeOpen::Ready);
                                self.ask_focus(FocusAsked::Open {
                                    beside: Beside::Alone,
                                    opens: Opens::Shell,
                                });
                            }
                            Some(WorktreeOpen::Requested) => {}
                            Some(WorktreeOpen::Ready) => emptied = true,
                        }
                    }
                }
                self.watch();
                self.sync_browsers();
                self.send_unsent();
                let became_live = !was_live && self.show.is_live();
                match (emptied, followed, laid_changed || became_live) {
                    (true, ..) => Heeded::Emptied,
                    (false, true, _) => Heeded::Followed,
                    (false, false, true) => Heeded::Changed,
                    (false, false, false) => Heeded::Quiet,
                }
            }
            Heard::Edged {
                worktree,
                direction,
                step,
                heard,
            } => {
                match self
                    .layout
                    .heard_edged(worktree, direction, step, heard, Instant::now())
                {
                    EdgeHeard::Crosses(across) => Heeded::Edged(across),
                    EdgeHeard::Stayed => {
                        self.send_unsent();
                        Heeded::Quiet
                    }
                    EdgeHeard::Ignored => Heeded::Quiet,
                }
            }
            Heard::Sent(worktree) => Heeded::Sent(worktree),
            Heard::Left(worktree) => {
                self.opened.remove(&worktree);
                if !self.layout.left(worktree, Instant::now()) {
                    return Heeded::Quiet;
                }
                self.watch();
                self.sync_browsers();
                Heeded::Emptied
            }
            Heard::Refused { why, heard } => {
                self.layout.heard_refused(heard, Instant::now());
                Heeded::Note(format!("luned refused: {why}"))
            }
            Heard::Forgotten(why) => {
                self.lose_stream(Instant::now());
                Heeded::Forgotten(why)
            }
            Heard::Lost(why) => {
                self.lose_stream(Instant::now());
                Heeded::Lost(why)
            }
        }
    }

    fn watch(&mut self) {
        let wanted = self.layout.wanted();
        match &mut self.watcher {
            Some(watcher) if watcher.showing == wanted => {}
            Some(watcher) => {
                watcher.viewer.show(wanted.clone());
                watcher.showing = wanted;
            }
            None if wanted.is_empty() => {}
            None if self.watched_at.is_some_and(|at| at.elapsed() < AGAIN) => {}
            None => {
                self.watchers += 1;
                self.watched_at = Some(Instant::now());
                let (number, watcher, events) = (self.number, self.watchers, self.events.clone());
                let reach = match self.reach() {
                    Ok(reach) => reach,
                    Err(why) => {
                        let _ = events.send(Event::Seen {
                            number,
                            watcher,
                            seen: Seen::Lost(why),
                        });
                        return;
                    }
                };
                let greeting = self.greeting;
                let told = move |seen| {
                    drop(events.send(Event::Seen {
                        number,
                        watcher,
                        seen,
                    }));
                };
                let viewer = viewer::start(reach, greeting, told);
                viewer.show(wanted.clone());
                self.watcher = Some(Watcher {
                    number: watcher,
                    viewer,
                    showing: wanted,
                });
            }
        }
    }

    fn reach(&self) -> Result<Reach, String> {
        let destination = match self.at() {
            Where::Here { runs_in } => {
                let socket = luned::mux::own::server::socket();
                let socket = socket.map_err(|failure| failure.to_string())?;
                return Ok(Reach::Mux { socket, runs_in });
            }
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
            }
        };
        if !watching.showing.contains(&framed.frame().terminal()) {
            return Heeded::Quiet;
        }
        framed.take(|frame| match self.layout.framed(frame) {
            Ok(true) => Heeded::Changed,
            Ok(false) => Heeded::Quiet,
            Err(why) => {
                self.unwatch();
                Heeded::Note(format!("lune-mux said what lune cannot follow: {why}"))
            }
        })
    }

    fn unwatch(&mut self) {
        self.watcher = None;
        self.layout.screens.clear();
    }

    pub fn ticked(&mut self) {
        if self.show.is_disconnected() && self.shown_at.elapsed() >= AGAIN {
            self.shown();
        }
        if self.watcher.is_none() {
            self.watch();
            self.send_unsent();
        }
    }

    fn ask(&mut self, asked: LayoutAsked) {
        if let Some(show) = self.show.active() {
            show.ask(asked);
        }
    }

    fn ask_focus(&mut self, asked: FocusAsked) {
        let step = match &asked {
            FocusAsked::Step { step, .. } => Some(*step),
            FocusAsked::Go(_)
            | FocusAsked::Open { .. }
            | FocusAsked::Welcome
            | FocusAsked::Browser(_)
            | FocusAsked::Review
            | FocusAsked::Close(_)
            | FocusAsked::Focus(_) => None,
        };
        if let Some(show) = self.show.active_mut() {
            let ask = show.ask_focus(asked);
            let waiting = match step {
                Some(step) => WaitingFor::Step { ask, step },
                None => WaitingFor::Focus(ask),
            };
            self.layout.await_focus(waiting, Instant::now());
        }
    }

    pub fn focus_view(&mut self, view: ViewId) {
        self.ask_focus(FocusAsked::Focus(view));
    }

    pub fn grow(&mut self, growth: Growth) {
        self.ask(LayoutAsked::Resize(growth));
    }

    pub fn step(&mut self, direction: Direction) {
        self.steps = self.steps.checked_add(1).expect("steps fit in u64");
        let step = NonZeroU64::new(self.steps).expect("steps count up from 1");
        self.ask_focus(FocusAsked::Step { direction, step });
    }

    pub fn forget_step(&mut self) {
        self.layout.forget_step(Instant::now());
    }

    pub fn stayed_at_edge(&mut self) {
        self.layout.stayed_at_edge(Instant::now());
        self.send_unsent();
    }

    pub fn crossed_edge(&mut self) {
        self.layout.crossed_edge(Instant::now());
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
        self.layout.go(worktree, Instant::now());
        self.browsers.clear();
        let _ = self.browsers.drain_tty_changes();
        self.ask_focus(FocusAsked::Go(worktree));
        self.watch();
        self.browsers.drain_all_deletes()
    }

    fn mark_opening(&mut self) {
        let state = if self.layout.laid().is_some_and(|laid| !laid.is_empty()) {
            WorktreeOpen::Ready
        } else {
            WorktreeOpen::Requested
        };
        self.opened.entry(self.layout.worktree).or_insert(state);
    }

    pub fn welcome(&mut self) {
        self.mark_opening();
        self.ask_focus(FocusAsked::Welcome);
    }

    pub fn open_browser(&mut self, url: Option<BrowserUrl>) {
        self.mark_opening();
        self.ask_focus(FocusAsked::Browser(url));
    }

    pub fn review(&mut self) {
        self.mark_opening();
        self.ask_focus(FocusAsked::Review);
    }

    #[must_use]
    pub fn diff_loaded(
        &mut self,
        worktree: WorktreeId,
        loaded: &Result<Arc<LoadedDiff>, String>,
        revision: NonZeroU64,
    ) -> bool {
        if self.layout.worktree != worktree {
            return false;
        }
        let mut changed = false;
        for pane in self.layout.reviews.values_mut() {
            changed |= pane.loaded(loaded.clone(), revision);
        }
        changed
    }

    pub fn arrange(&mut self, arranged: Arranged) {
        self.ask(LayoutAsked::Arrange(arranged));
    }

    pub fn open_beside_focused(&mut self, opens: Opens) {
        self.mark_opening();
        self.ask_focus(FocusAsked::Open {
            beside: Beside::Focused,
            opens,
        });
    }

    #[must_use]
    pub fn tabs(
        &mut self,
        worktree_tabs: Option<WorktreeTabs>,
        titles: BTreeMap<ViewId, String>,
    ) -> bool {
        let changed = self.layout.worktree_tabs != worktree_tabs || self.layout.titles != titles;
        self.layout.worktree_tabs = worktree_tabs;
        self.layout.titles = titles;
        changed
    }

    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) {
        let size = layout_size(columns, rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.ask(LayoutAsked::Size(size));
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
        assert!(
            watcher.showing.contains(&key),
            "a watcher shows each terminal laid out"
        );
        for event in events {
            watcher.viewer.typed(key, event);
        }
    }

    #[must_use]
    pub fn key(&mut self, key: &KeyEvent, input: u64) -> (Heeded, ReviewAction) {
        let unselected = self.unselect();
        let plain = if unselected {
            Heeded::Changed
        } else {
            Heeded::Quiet
        };
        if self.layout.held_after_close(key) {
            return (plain, ReviewAction::None);
        }
        if self.layout.welcomes() {
            if let Some(view) = self.layout.closes(key, input) {
                self.ask_focus(FocusAsked::Close(view));
            }
            return (plain, ReviewAction::None);
        }
        if let Some((view, url, inner)) = self.layout.focused_browser() {
            let url = url.cloned();
            let handled = self.browsers.key(view, key);
            self.flush_browser_ttys();
            if handled {
                return (plain, ReviewAction::None);
            }
            if let Some(closed) = self.layout.closes(key, input) {
                self.ask_focus(FocusAsked::Close(closed));
                return (plain, ReviewAction::None);
            }
            let pressed = key.kind == KeyEventKind::Press && key.modifiers.is_empty();
            if pressed {
                match key.code {
                    KeyCode::Char('o') => {
                        if let Some(url) = url {
                            return (Heeded::OpenOsBrowser(url), ReviewAction::None);
                        }
                    }
                    KeyCode::Char('y') => {
                        if let Some(url) = url {
                            return (Heeded::Copy(url.as_str().to_owned()), ReviewAction::None);
                        }
                    }
                    KeyCode::Char('r') if self.browsers.supported() => {
                        self.browsers.retry(
                            view,
                            &self.machine,
                            url.as_ref(),
                            inner,
                            self.window_focused,
                        );
                        self.flush_browser_ttys();
                        return (Heeded::Changed, ReviewAction::None);
                    }
                    _ => {}
                }
            }
            return (plain, ReviewAction::None);
        }
        if self.layout.reviews() {
            let Some(view) = self.layout.focused() else {
                return (plain, ReviewAction::None);
            };
            let searching = self
                .layout
                .reviews
                .get(&view)
                .is_some_and(ReviewPane::is_searching);
            if searching {
                self.layout.disarm_close();
            } else if let Some(closed) = self.layout.closes(key, input) {
                self.ask_focus(FocusAsked::Close(closed));
                return (plain, ReviewAction::None);
            }
            let action = self
                .layout
                .reviews
                .get_mut(&view)
                .map_or(ReviewAction::None, |pane| pane.key(key));
            let heeded = if key.kind != KeyEventKind::Release || unselected {
                Heeded::Changed
            } else {
                Heeded::Quiet
            };
            return (heeded, action);
        }
        if let Some(key) = key_said(key) {
            self.type_focused(typed::Event::Key(key));
        }
        (plain, ReviewAction::None)
    }

    #[must_use]
    pub fn paste(&mut self, text: &str) -> bool {
        let unselected = self.unselect();
        if let Some((view, _, _)) = self.layout.focused_browser() {
            let _ = self.browsers.paste(view, text);
            self.flush_browser_ttys();
            return unselected;
        }
        self.type_focused(typed::Event::Paste(api::Paste {
            text: text.to_owned(),
        }));
        unselected
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
                self.focus_view(terminal);
                None
            }
            Moused::Sent(key, said) => {
                self.type_in(key, typed::Event::Mouse(said));
                None
            }
            Moused::Browser {
                view,
                mouse,
                column,
                row,
            } => {
                let _ = self.browsers.mouse(view, mouse, column, row);
                self.flush_browser_ttys();
                None
            }
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

    #[must_use]
    pub fn unselect(&mut self) -> bool {
        let before = self.layout.pointer;
        self.layout.pointer = before.unselected();
        self.layout.pointer != before
    }

    #[must_use]
    pub(crate) fn pointed(&self, at: Spot) -> Option<Selection> {
        self.layout.pointed(at)
    }

    #[must_use]
    pub fn draw(
        &mut self,
        clean: Clean<'_>,
        hover: Option<Spot>,
        kitty_cell_px: Option<(NonZeroU16, NonZeroU16)>,
    ) -> (Option<Position>, Vec<RibbonOverlay>) {
        self.layout
            .draw(clean, hover, Some(&self.browsers), kitty_cell_px)
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
    use luned::mux::own::OwnMade;
    use luned::mux::own::frames::{Cursor, Ending, Glyph, Grapheme, Line, Rows, Shows, Side};
    use ratatui::style::Modifier;

    use super::*;
    use crate::attached::At;

    fn side(count: u16) -> Side {
        Side::new(count).unwrap()
    }

    fn narrow(it: &str) -> Glyph {
        let shows = Shows::Narrow(Grapheme::new(it.to_owned()).unwrap());
        Glyph {
            shows,
            style: frames::Style::PLAIN,
        }
    }

    fn lines(rows: &[&str]) -> Vec<Line> {
        let line = |row: &str| {
            let glyphs: Vec<Glyph> = row.chars().map(|it| narrow(&it.to_string())).collect();
            Line::new(
                glyphs,
                Ending::Here,
                side(u16::try_from(row.len()).unwrap()),
            )
            .unwrap()
        };
        rows.iter().map(|row| line(row)).collect()
    }

    fn key(id: u32) -> TerminalKey {
        TerminalKey {
            id: NonZeroU32::new(id).unwrap(),
            made: OwnMade::from_nanos(0),
        }
    }

    fn whole(id: u32, rows: &[&str], cursor: Option<Cursor>) -> Frame {
        let lines = lines(rows);
        let columns = side(u16::try_from(rows[0].len()).unwrap());
        let size = frames::Size {
            columns,
            rows: side(u16::try_from(lines.len()).unwrap()),
        };
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
            rect: Some(show_response::Rect {
                column,
                row,
                columns,
                rows,
            }),
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
            size: Some(show_request::Size {
                columns: 20,
                rows: 4,
            }),
            views,
            shown: Some(if full {
                Said::Full(focused)
            } else {
                Said::Tiled(focused)
            }),
            version: None,
        })
        .unwrap()
    }

    fn halves() -> Vec<show_response::View> {
        vec![
            view(11, (0, 0, 10, 4), Some(1)),
            view(12, (10, 0, 10, 4), Some(2)),
        ]
    }

    fn split_layout() -> Layout {
        let mut layout = Layout::new(worktree(1), None);
        let tabs = vec![
            Tab {
                worktree: worktree(1),
                name: "one".to_owned(),
            },
            Tab {
                worktree: worktree(2),
                name: "two".to_owned(),
            },
        ];
        let project = ProjectId::of(2).unwrap();
        assert_eq!(colour_of(project), moonlight::MAUVE);
        layout.worktree_tabs = Some(WorktreeTabs { project, tabs });
        layout.titles = [
            (terminal(11), "p1".to_owned()),
            (terminal(12), "p2".to_owned()),
        ]
        .into();
        assert_eq!(layout.laid_out(laid(halves(), 11, false)), LaidOut::Shown);
        let cursor = Cursor {
            row: 1,
            column: 2,
            shape: frames::CursorShape::Block,
            blinking: false,
        };
        layout
            .framed(whole(1, &["abcdefgh", "ijklmnop"], Some(cursor)))
            .unwrap();
        layout
            .framed(whole(2, &["qrstuvwx", "yz012345"], None))
            .unwrap();
        layout
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn spot(row: u16, column: u16) -> Spot {
        Spot { row, column }
    }

    fn row(buffer: &Buffer, y: u16, from: u16, to: u16) -> String {
        (from..to).map(|x| buffer[(x, y)].symbol()).collect()
    }

    #[test]
    fn each_terminal_draws_in_its_frame_under_the_worktrees_and_a_larger_area_keeps_a_margin() {
        let mut layout = split_layout();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
        let (cursor, _) = layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(2, 0, 22, 6)),
            None,
            None,
            None,
        );
        assert_eq!(row(&buffer, 0, 2, 17), "\u{e0b6} one \u{e0b4}   two  ");
        assert_eq!(
            (buffer[(3, 0)].fg, buffer[(3, 0)].bg),
            (moonlight::INK, moonlight::MAUVE)
        );
        assert_eq!(
            (buffer[(2, 0)].fg, buffer[(2, 0)].bg),
            (moonlight::MAUVE, Color::Reset)
        );
        assert_eq!(
            (buffer[(12, 0)].fg, buffer[(12, 0)].bg),
            (moonlight::MUTE, Color::Reset)
        );
        assert_eq!(row(&buffer, 1, 2, 22), "╭ p1 ────╮╭ p2 ────╮");
        assert_eq!(row(&buffer, 2, 2, 12), "│abcdefgh│");
        assert_eq!(row(&buffer, 3, 12, 22), "│yz012345│");
        assert_eq!(row(&buffer, 4, 2, 22), "╰────────╯╰────────╯");
        assert_eq!(
            (buffer[(2, 1)].fg, buffer[(12, 1)].fg),
            (moonlight::MUTE, moonlight::LIFTED)
        );
        assert_eq!(row(&buffer, 5, 2, 24), " ".repeat(22));
        assert_eq!(cursor, Some(Position::new(5, 3)));
        let mut small = Buffer::empty(Rect::new(0, 0, 14, 3));
        assert_eq!(
            layout
                .draw(
                    Clean::of_empty(&mut small, Rect::new(0, 0, 14, 3)),
                    None,
                    None,
                    None
                )
                .0,
            None
        );
        assert_eq!(row(&small, 1, 0, 14), "╭ p1 ────╮╭ p2");
        assert_eq!(row(&small, 2, 0, 14), "│abcdefgh││qrs");
    }

    #[test]
    fn a_view_of_the_worktree_left_is_not_drawn_in_the_one_gone_to() {
        let mut layout = split_layout();
        layout.go(worktree(2), Instant::now());
        let drawn = layout.drawn().is_empty() && layout.wanted().is_empty();
        assert!(drawn && layout.screens.is_empty());
        assert_eq!(layout.laid_out(laid(halves(), 11, false)), LaidOut::Ignored);
        assert!(layout.drawn().is_empty() && layout.focused().is_none());
    }

    #[test]
    fn an_edge_is_heeded_only_from_the_worktree_the_view_is_on() {
        let mut layout = split_layout();
        assert_eq!(
            layout.edged(worktree(1), Direction::Right),
            Some(Across::After)
        );
        layout.go(worktree(2), Instant::now());
        assert_eq!(
            layout.edged(worktree(1), Direction::Right),
            None,
            "an edge of the one left"
        );
        assert_eq!(
            layout.edged(worktree(2), Direction::Left),
            Some(Across::Before)
        );
    }

    #[test]
    fn a_full_terminal_fills_the_view_and_a_lost_one_or_the_one_lune_runs_in_says_so() {
        let mut layout = split_layout();
        layout.laid_out(laid(halves(), 12, true));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            None,
            None,
            None,
        );
        assert_eq!(row(&buffer, 1, 0, 20), "╭ p2 ──────────────╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        let mut here = Layout::new(worktree(1), Some(key(2)));
        let lost = vec![
            view(11, (0, 0, 10, 4), None),
            view(12, (10, 0, 10, 4), Some(2)),
        ];
        here.laid_out(laid(lost, 11, false));
        assert_eq!(
            here.wanted(),
            BTreeSet::new(),
            "lune never shows the terminal it runs in"
        );
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        here.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            None,
            None,
            None,
        );
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
                size: Some(show_request::Size {
                    columns: 20,
                    rows: 4,
                }),
                views: vec![tab(11, Some(1)), tab(12, Some(2)), tab(13, None)],
                shown: Some(Said::Tabbed(focused)),
                version: None,
            })
            .unwrap()
        };
        layout.laid_out(tabs(12));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            None,
            None,
            None,
        );
        assert_eq!(row(&buffer, 1, 0, 20), "╭  p  ─ ● p ─  t  ─╮");
        assert_eq!(row(&buffer, 2, 0, 10), "│qrstuvwx ");
        assert_eq!(
            (buffer[(8, 1)].symbol(), buffer[(8, 1)].fg),
            (VIEW_DOT, moonlight::MAUVE)
        );
        assert_eq!(buffer[(10, 1)].fg, moonlight::TEXT);
        assert!(buffer[(10, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(
            buffer[(3, 1)].fg,
            moonlight::MUTE,
            "a tab not focused is dim"
        );
        let down = mouse(MouseEventKind::Down(MouseButton::Left));
        assert_eq!(
            layout.moused(down, spot(1, 2), 20, None),
            Moused::Focus(terminal(11))
        );
        assert_eq!(
            layout.moused(down, spot(1, 14), 20, None),
            Moused::Focus(terminal(13))
        );
        let on_the_focused = layout.moused(down, spot(1, 7), 20, None);
        assert_eq!(on_the_focused, Moused::Nothing, "the focused tab is there");
        assert!(!matches!(
            layout.moused(down, spot(2, 3), 20, None),
            Moused::Focus(_)
        ));
    }

    #[test]
    fn tabs_share_the_border_and_the_focused_one_is_always_drawn() {
        use show_response::laid::Shown as Said;
        let mut layout = split_layout();
        let long = |title: &str| title.to_owned();
        layout.titles = [
            (terminal(11), long("a very long title")),
            (terminal(12), long("another long one")),
        ]
        .into();
        let tab = |id, key| view(id, (0, 0, 20, 4), key);
        let tabs = Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size {
                columns: 20,
                rows: 4,
            }),
            views: vec![tab(11, Some(1)), tab(12, Some(2)), tab(13, None)],
            shown: Some(Said::Tabbed(13)),
            version: None,
        });
        layout.laid_out(tabs.unwrap());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            None,
            None,
            None,
        );
        assert_eq!(row(&buffer, 1, 0, 20), "╭  a  ─  a  ─ ● t ─╮");
        let drawn = |columns| {
            let spans = layout.view_tabs(Rect::new(0, 0, columns, 4), None);
            spans
                .iter()
                .map(|span| (span.id, span.column, span.columns))
                .collect::<Vec<_>>()
        };
        let (one, two, three) = (terminal(11), terminal(12), terminal(13));
        assert_eq!(
            drawn(7),
            [(one, 0, 1), (two, 2, 1), (three, 4, 1)],
            "a column each"
        );
        assert_eq!(
            drawn(6),
            [(two, 0, 1), (three, 2, 1)],
            "the tabs before it go first"
        );
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
        assert_eq!(
            layout.moused(down, spot(2, 14), 22, None),
            Moused::Focus(terminal(12))
        );
        assert_eq!(
            layout.moused(down, spot(0, 9), 22, None),
            Moused::Go(worktree(2))
        );
        assert_eq!(layout.moused(down, spot(0, 1), 22, None), Moused::Nothing);
        assert_eq!(layout.moused(down, spot(1, 0), 22, None), Moused::Nothing);
        assert_eq!(
            at(layout.moused(down, spot(2, 3), 22, None)),
            Some((1, 0, 2))
        );
        assert_eq!(
            at(layout.moused(drag, spot(0, 15), 22, None)),
            Some((1, 0, 7))
        );
        assert_eq!(
            at(layout.moused(up, spot(4, 15), 22, None)),
            Some((1, 1, 7))
        );
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
        layout.framed(whole(2, &["ab"], None)).unwrap();
        assert!(matches!(layout.pointer, Pointer::Shown(_)));
        layout.framed(whole(1, &["ab"], None)).unwrap();
        assert_eq!(layout.pointer, Pointer::Free);
    }

    #[test]
    fn a_click_on_a_link_copies_it_and_the_pointer_over_one_underlines_it() {
        let mut layout = split_layout();
        layout
            .framed(whole(1, &["a/b.rs x", "ijklmnop"], None))
            .unwrap();
        layout.select_from(spot(2, 3));
        assert_eq!(layout.selected().as_deref(), Some("a/b.rs"));
        let lit = Selection {
            from: At { row: 0, column: 0 },
            to: At { row: 0, column: 5 },
        };
        assert_eq!(layout.pointer, Pointer::Shown(lit));
        layout.select_from(spot(2, 3));
        layout.select_to(spot(2, 3));
        assert_eq!(
            layout.selected().as_deref(),
            Some("a/b.rs"),
            "a press that stays is a click"
        );
        layout.select_from(spot(2, 3));
        layout.select_to(spot(2, 6));
        layout.select_to(spot(2, 3));
        assert_eq!(
            layout.selected(),
            None,
            "a drag that comes back is no click"
        );
        assert_eq!(layout.pointer, Pointer::Free);
        layout.select_from(spot(2, 8));
        assert_eq!(layout.selected(), None, "a click on a word copies nothing");
        assert_eq!(layout.pointer, Pointer::Free);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            Some(spot(2, 2)),
            None,
            None,
        );
        let underlined = |x| buffer[(x, 2)].modifier.contains(Modifier::UNDERLINED);
        let line: String = (1..9)
            .map(|x| if underlined(x) { '_' } else { ' ' })
            .collect();
        assert_eq!(line, "______  ");
        assert_eq!(
            layout.pointed(spot(2, 13)),
            None,
            "only in the focused terminal"
        );
        let size = frames::Size {
            columns: side(8),
            rows: side(2),
        };
        let rows = Rows::Whole(lines(&["a/b.rs x", "ijklmnop"]));
        layout
            .framed(Frame::new(key(1), size, rows, None, true).unwrap())
            .unwrap();
        let taken = "where the program takes the mouse, none shows";
        assert_eq!(layout.pointed(spot(2, 2)), None, "{taken}");
        layout.framed(whole(1, &["x a/b.rs"], None)).unwrap();
        let past = "past a screen shorter than its view, none shows";
        assert_eq!(layout.pointed(spot(3, 8)), None, "{past}");
        layout.select_from(spot(3, 8));
        assert_eq!(layout.selected(), None, "nor does a click there copy");
        assert_eq!(layout.pointer, Pointer::Free);
        layout.select_from(spot(3, 8));
        layout.select_to(spot(2, 3));
        assert_eq!(
            layout.selected().as_deref(),
            Some("a/b.rs"),
            "a drag from there still selects"
        );
    }

    #[test]
    fn changed_rows_follow_only_a_frame_of_their_size_and_a_terminal_laid_out_no_more_drops() {
        let mut layout = split_layout();
        let changed = Rows::Changed(vec![(0, lines(&["c"]).remove(0))]);
        let size = frames::Size {
            columns: side(1),
            rows: side(1),
        };
        let frame = Frame::new(key(1), size, changed, None, false).unwrap();
        let why = layout.framed(frame).unwrap_err();
        assert_eq!(why, "a frame of 1x1 changes the rows of 8x2");
        layout.laid_out(laid(vec![view(12, (0, 0, 20, 4), Some(2))], 12, false));
        assert_eq!(layout.screens.keys().copied().collect::<Vec<_>>(), [key(2)]);
        layout.framed(whole(1, &["no"], None)).unwrap();
        let lacked = "a terminal its layout lacks gets no screen";
        assert!(!layout.screens.contains_key(&key(1)), "{lacked}");
        layout.go(worktree(2), Instant::now());
        assert_eq!(
            layout.focused(),
            None,
            "a layout of another worktree lays out nothing here"
        );
    }

    #[test]
    fn a_frame_moves_its_glyphs_into_the_screen_without_cloning_and_replaces_only_changed_rows() {
        let ptr_of = |shows: &Shows| match shows {
            Shows::Narrow(grapheme) | Shows::Wide(grapheme) => grapheme.as_str().as_ptr(),
            Shows::Blank => panic!("expected a grapheme"),
        };
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(layout.laid_out(laid(halves(), 11, false)), LaidOut::Shown);
        let first = whole(1, &["abcdefgh", "ijklmnop"], None);
        let Rows::Whole(initial) = first.rows() else {
            unreachable!()
        };
        let row0_ptr = ptr_of(&initial[0].glyphs()[0].shows);
        let row1_ptr = ptr_of(&initial[1].glyphs()[0].shows);
        layout.framed(first).unwrap();
        let screen = &layout.screens[&key(1)];
        assert_eq!(ptr_of(&screen.lines()[0].glyphs()[0].shows), row0_ptr);
        assert_eq!(ptr_of(&screen.lines()[1].glyphs()[0].shows), row1_ptr);

        let updated = lines(&["QRSTUVWX"]).remove(0);
        let updated_ptr = ptr_of(&updated.glyphs()[0].shows);
        let cursor = Some(Cursor {
            row: 1,
            column: 3,
            shape: frames::CursorShape::Bar,
            blinking: true,
        });
        let size = frames::Size {
            columns: side(8),
            rows: side(2),
        };
        let changed = Frame::new(
            key(1),
            size,
            Rows::Changed(vec![(1, updated)]),
            cursor,
            true,
        );
        layout.framed(changed.unwrap()).unwrap();
        let screen = &layout.screens[&key(1)];
        assert_eq!(
            ptr_of(&screen.lines()[0].glyphs()[0].shows),
            row0_ptr,
            "row 0 stayed in place"
        );
        assert_eq!(
            ptr_of(&screen.lines()[1].glyphs()[0].shows),
            updated_ptr,
            "row 1 moved in"
        );
        assert_eq!(screen.cursor(), cursor);
        assert!(screen.tracks_mouse());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        let (cursor_at, _) = layout.draw(
            Clean::of_empty(&mut buffer, Rect::new(0, 0, 20, 5)),
            None,
            None,
            None,
        );
        assert_eq!(row(&buffer, 2, 1, 9), "abcdefgh");
        assert_eq!(row(&buffer, 3, 1, 9), "QRSTUVWX");
        assert_eq!(cursor_at, Some(Position::new(4, 3)));
    }

    #[test]
    fn drawing_a_layout_leaves_untouched_cells_in_its_area_alone() {
        let mut layout = split_layout();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
        for spot in [(2, 5), (22, 2), (19, 0)] {
            buffer[spot].set_symbol("x");
        }
        let _ = layout.draw(
            Clean::unchecked(&mut buffer, Rect::new(2, 0, 22, 6)),
            None,
            None,
            None,
        );
        for spot in [(2, 5), (22, 2), (19, 0)] {
            assert_eq!(
                buffer[spot].symbol(),
                "x",
                "layout draw does not reset untouched cells"
            );
        }
        assert_eq!(row(&buffer, 2, 2, 12), "│abcdefgh│");
    }

    fn laid_in(id: u64, views: Vec<show_response::View>) -> Heard {
        use show_response::laid::Shown as Said;
        let shown = views.first().map(|view| Said::Tiled(view.id));
        let size = Some(show_request::Size {
            columns: 20,
            rows: 4,
        });
        let said = show_response::Laid {
            worktree: id,
            size,
            views,
            shown,
            version: None,
        };
        Heard::Laid {
            laid: Laid::heard(said).unwrap(),
            heard: Echoed::Through(AskNumber::HELLO),
        }
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
        assert_eq!(layout.laid_out(laid(views(), 12, false)), LaidOut::Shown);
        let typed = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let d = typed(KeyCode::Char(CLOSE));
        let welcome = Some(ViewId::of(12).unwrap());
        assert_eq!(layout.closes(&d, 1), None);
        assert_eq!(layout.closes(&d, 2), welcome, "d d closes it, by its id");
        assert_eq!(layout.closes(&d, 3), None);
        assert_eq!(layout.closes(&typed(KeyCode::Char('x')), 4), None);
        assert_eq!(
            layout.closes(&d, 5),
            None,
            "another key between the two starts over"
        );
        let ctrl_d = KeyEvent::new(KeyCode::Char(CLOSE), KeyModifiers::CONTROL);
        assert_eq!(layout.closes(&ctrl_d, 6), None);
        assert_eq!(layout.closes(&d, 7), None, "Ctrl d is no d");
        assert_eq!(layout.laid_out(laid(views(), 11, false)), LaidOut::Shown);
        assert_eq!(layout.laid_out(laid(views(), 12, false)), LaidOut::Shown);
        assert_eq!(
            layout.closes(&d, 8),
            None,
            "a focus gone elsewhere and back starts over"
        );
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
        assert_eq!(
            layout.laid_out(laid(vec![view(11, (0, 0, 20, 4), Some(1))], 11, false)),
            LaidOut::Shown,
        );
        assert!(
            layout.held_after_close(&held),
            "the d held after the close is dropped"
        );
        assert!(layout.held_after_close(&held));
        assert!(
            !layout.held_after_close(&d),
            "a d pressed again reaches the terminal"
        );
        assert!(!layout.held_after_close(&held));
    }

    #[test]
    fn a_worktree_emptied_is_left_and_one_gone_back_to_opens_again() {
        let unserved = tempfile::tempdir().unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(NonZeroU16::new(20).unwrap(), NonZeroU16::new(6).unwrap());
        let greeting = Greeting {
            foreground: None,
            background: None,
        };
        let folder = unserved.path().to_owned();
        let browser_config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: Colours::default(),
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
        assert!(
            matches!(viewed.heard(laid_in(1, Vec::new())), Heeded::Changed),
            "first, it opens"
        );
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Changed));
        let emptied = viewed.heard(laid_in(1, Vec::new()));
        assert!(
            matches!(emptied, Heeded::Emptied),
            "its last terminal closed, lune leaves it"
        );
        viewed.go(&Machine::Local, worktree(2));
        let two = vec![view(21, (0, 0, 20, 4), Some(2))];
        assert!(matches!(viewed.heard(laid_in(2, two)), Heeded::Changed));
        viewed.go(&Machine::Local, worktree(1));
        let back = viewed.heard(laid_in(1, Vec::new()));
        assert!(
            matches!(back, Heeded::Changed),
            "gone back to, it opens as it did first"
        );
        assert!(matches!(viewed.heard(laid_in(1, one())), Heeded::Changed));
        let again = viewed.heard(laid_in(1, Vec::new()));
        assert!(
            matches!(again, Heeded::Emptied),
            "emptied again, lune leaves it again"
        );
    }

    #[test]
    fn opening_browser_on_empty_worktree_waits_for_browser_before_allowing_emptied() {
        use show_response::view::{Browser, Found as Said};
        let unserved = tempfile::tempdir().unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(NonZeroU16::new(20).unwrap(), NonZeroU16::new(6).unwrap());
        let greeting = Greeting {
            foreground: None,
            background: None,
        };
        let browser_config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: Colours::default(),
        };
        let mut viewed = Viewed::start(
            1,
            unserved.path().to_owned(),
            Machine::Local,
            worktree(1),
            size,
            greeting,
            None,
            browser_config,
            &events,
        );
        viewed.open_browser(None);
        assert!(
            matches!(viewed.heard(laid_in(1, Vec::new())), Heeded::Changed),
            "initial empty Laid before Asked::Browser settles must not bounce user out"
        );
        let browser_view = show_response::View {
            id: 31,
            rect: Some(show_response::Rect {
                column: 0,
                row: 0,
                columns: 20,
                rows: 4,
            }),
            found: Some(Said::Browser(Browser { url: None })),
        };
        assert!(matches!(
            viewed.heard(laid_in(1, vec![browser_view])),
            Heeded::Changed
        ));
        assert!(
            matches!(viewed.heard(laid_in(1, Vec::new())), Heeded::Emptied),
            "closing the browser later leaves the emptied worktree"
        );
    }

    fn pasted(text: &str) -> typed::Event {
        typed::Event::Paste(api::Paste {
            text: text.to_owned(),
        })
    }

    fn found(terminal: u64, said: show_response::view::Found) -> show_response::View {
        let mut it = view(terminal, (0, 0, 20, 4), None);
        it.found = Some(said);
        it
    }

    fn laid_on(id: u64, focused: show_response::View) -> Laid {
        let Heard::Laid { laid, .. } = laid_in(id, vec![focused]) else {
            unreachable!()
        };
        laid
    }

    fn sent(layout: &mut Layout, now: Instant) -> Option<(TerminalKey, VecDeque<typed::Event>)> {
        layout
            .take_unsent(Some(()), now)
            .map(|((), key, events)| (key, events))
    }

    #[test]
    fn events_typed_before_a_terminal_wait_for_it_and_go_in_order_once_it_is_shown() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), now);
        assert_eq!(sent(&mut layout, now), None, "no layout yet");
        assert_eq!(
            layout.laid_out(laid_on(1, found(11, Said::Looking(())))),
            LaidOut::Shown
        );
        assert_eq!(
            sent(&mut layout, now),
            None,
            "its terminal is yet to be found"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        assert_eq!(layout.take_unsent(None::<()>, now), None, "no viewer yet");
        let sent_now = sent(&mut layout, now);
        assert_eq!(
            sent_now,
            Some((key(1), VecDeque::from([pasted("a"), pasted("b")])))
        );
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
            assert_eq!(layout.laid_out(laid_on(1, focused)), LaidOut::Shown);
            assert_eq!(sent(&mut layout, now), None);
            assert_eq!(layout.laid_out(laid_on(1, terminal())), LaidOut::Shown);
            assert_eq!(sent(&mut layout, now), Some((key(1), VecDeque::new())));
        }
        let mut layout = Layout::new(worktree(1), Some(key(1)));
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(layout.laid_out(laid_on(1, terminal())), LaidOut::Shown);
        assert_eq!(sent(&mut layout, now), None, "lune types not into itself");
        assert!(matches!(layout.unsent.held, Held::Nothing));
    }

    #[test]
    fn events_kept_for_a_terminal_never_reach_the_one_put_in_its_view_in_its_place() {
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(
            layout.take_unsent(None::<()>, now),
            None,
            "its viewer is lost"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::new())),
            "remade"
        );
    }

    #[test]
    fn events_kept_for_one_worktree_never_reach_another_gone_to_or_followed() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        layout.keep_unsent(pasted("a"), now);
        layout.go(worktree(2), now);
        assert_eq!(
            layout.laid_out(laid_on(2, view(21, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::new())),
            "gone to"
        );
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, found(11, Said::Looking(())))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("a"), now);
        let followed = laid_on(2, view(21, (0, 0, 20, 4), Some(2)));
        assert!(layout.followed(&followed, now));
        assert_eq!(layout.laid_out(followed), LaidOut::Shown);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::new())),
            "followed"
        );
    }

    #[test]
    fn events_kept_for_one_view_never_reach_another_focused_in_its_place() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, found(11, Said::Looking(())))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())));
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(layout.take_unsent(None::<()>, now), None, "no viewer yet");
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("b"), now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::from([pasted("b")])))
        );
    }

    #[test]
    fn events_kept_longer_than_the_most_time_are_dropped_with_those_after_them() {
        let now = Instant::now();
        let later = now + UNSENT_FOR;
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(
            sent(&mut layout, later),
            Some((key(1), VecDeque::from([pasted("a")])))
        );
        let too_late = later + Duration::from_millis(1);
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), later);
        assert_eq!(sent(&mut layout, too_late), Some((key(1), VecDeque::new())));
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), too_late);
        layout.keep_unsent(pasted("c"), too_late);
        assert_eq!(
            sent(&mut layout, too_late),
            Some((key(1), VecDeque::new())),
            "no tail"
        );
        layout.keep_unsent(pasted("d"), too_late);
        let taken = Some((key(1), VecDeque::from([pasted("d")])));
        assert_eq!(
            sent(&mut layout, too_late),
            taken,
            "a terminal took input, so on it goes"
        );
    }

    #[test]
    fn events_past_the_most_kept_are_dropped_and_those_kept_stay_in_order() {
        let now = Instant::now();
        let aim = Aim {
            worktree: worktree(1),
            focus: AimFocus::Unlaid,
        };
        let mut unsent = Unsent::new(aim);
        for index in 0..=UNSENT_MAX {
            unsent.keep(aim, pasted(&index.to_string()), now);
        }
        assert!(matches!(&unsent.held, Held::Kept(kept) if kept.dropped));
        let kept: Vec<typed::Event> = unsent.take().into();
        let first = (0..UNSENT_MAX).map(|index| pasted(&index.to_string()));
        assert_eq!(kept, first.collect::<Vec<typed::Event>>());
        assert!(
            matches!(unsent.held, Held::Nothing),
            "a batch sent starts the count over"
        );
    }

    #[test]
    fn browser_view_draws_fallback_and_handles_open_copy_and_close_when_unsupported() {
        use show_response::view::{Browser, Found as Said};
        let mut layout = Layout::new(worktree(1), None);
        let browser = show_response::View {
            id: 31,
            rect: Some(show_response::Rect {
                column: 0,
                row: 0,
                columns: 48,
                rows: 10,
            }),
            found: Some(Said::Browser(Browser {
                url: Some("https://example.com/pull/809".to_owned()),
            })),
        };
        let laid_browser = Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size {
                columns: 48,
                rows: 10,
            }),
            views: vec![browser],
            shown: Some(show_response::laid::Shown::Tiled(31)),
            version: None,
        })
        .unwrap();
        assert_eq!(layout.laid_out(laid_browser), LaidOut::Shown);
        let (_, url, _) = layout.focused_browser().unwrap();
        assert_eq!(
            url.map(BrowserUrl::as_str),
            Some("https://example.com/pull/809")
        );

        let area = Rect::new(0, 0, 48, 12);
        let mut buffer = Buffer::empty(area);
        let (events, _rx) = std::sync::mpsc::channel();
        let config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: crate::answers::CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: crate::answers::Colours::default(),
        };
        let browsers = embedded::Browsers::new(1, config, events);
        let _ = layout.draw(
            Clean::of_empty(&mut buffer, area),
            None,
            Some(&browsers),
            None,
        );
        let rendered: String = (0..area.height)
            .map(|y| row(&buffer, y, 0, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("https://example.com/pull/809"),
            "{rendered}"
        );
        assert!(rendered.contains("o  open in OS browser"), "{rendered}");
        assert!(rendered.contains("y  copy URL"), "{rendered}");
    }

    #[test]
    fn a_review_view_renders_loads_diff_and_handles_split_ribbons_and_keys() {
        use show_response::view::Found as Said;
        let mut layout = Layout::new(worktree(1), None);
        let mut review_view = view(12, (0, 0, 112, 14), None);
        review_view.found = Some(Said::Review(()));
        let laid = Laid::heard(show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size {
                columns: 112,
                rows: 14,
            }),
            views: vec![review_view],
            shown: Some(show_response::laid::Shown::Tiled(12)),
            version: None,
        })
        .unwrap();
        assert_eq!(layout.laid_out(laid), LaidOut::Shown);
        assert!(layout.reviews());
        assert!(layout.has_review());
        assert!(layout.review_needs_load());

        let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn demo() {
-    old();
+    new();
 }
";
        let response = Arc::new(
            LoadedDiff::of_wire(luned::api::ReadDiffResponse {
                base: "main".to_owned(),
                head: "feat".to_owned(),
                patch: patch.as_bytes().to_vec(),
                truncated: false,
                untracked: Vec::new(),
            })
            .unwrap(),
        );
        for pane in layout.reviews.values_mut() {
            assert!(pane.loaded(Ok(Arc::clone(&response)), NonZeroU64::MIN));
        }
        assert!(!layout.review_needs_load());

        let area = Rect::new(0, 0, 112, 15);
        let mut buf = Buffer::empty(area);
        let (_, overlays_unicode) = layout.draw(Clean::of_empty(&mut buf, area), None, None, None);
        assert!(overlays_unicode.is_empty());

        let mut buf_kitty = Buffer::empty(area);
        let (_, overlays_kitty) = layout.draw(
            Clean::of_empty(&mut buf_kitty, area),
            None,
            None,
            Some((NonZeroU16::new(8).unwrap(), NonZeroU16::new(16).unwrap())),
        );
        assert_eq!(overlays_kitty.len(), 1);
        assert_eq!(overlays_kitty[0].image_id, review::KITTY_BASE_ID);

        let pane = layout.reviews.get_mut(&terminal(12)).unwrap();
        pane.key(&KeyEvent::from(KeyCode::Char('j')));
        pane.key(&KeyEvent::from(KeyCode::Char('j')));
        let action = pane.key(&KeyEvent::from(KeyCode::Char('e')));
        assert!(matches!(action, ReviewAction::Explain(ref ex) if ex.path() == "src/lib.rs"));

        let kind = |kind| KeyEvent::new_with_kind(KeyCode::Char(CLOSE), KeyModifiers::NONE, kind);
        let (d, held) = (kind(KeyEventKind::Press), kind(KeyEventKind::Repeat));
        assert_eq!(layout.closes(&d, 1), None);
        assert_eq!(
            layout.closes(&held, 2),
            None,
            "holding d does not close a review pane"
        );
        assert_eq!(layout.closes(&d, 3), None);
        assert_eq!(
            layout.closes(&d, 4),
            Some(terminal(12)),
            "d d closes the review pane"
        );
        assert!(
            layout.held_after_close(&held),
            "trailing repeat d is swallowed after close"
        );
    }

    #[test]
    fn framed_reports_whether_the_screen_changed() {
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        assert_eq!(layout.framed(whole(1, &["ab"], None)), Ok(true));
        assert_eq!(layout.framed(whole(1, &["ab"], None)), Ok(false));
        let size = frames::Size {
            columns: side(2),
            rows: side(1),
        };
        let same_row = Frame::new(
            key(1),
            size,
            Rows::Changed(vec![(0, lines(&["ab"]).pop().unwrap())]),
            None,
            false,
        )
        .unwrap();
        assert_eq!(layout.framed(same_row), Ok(false));
        let different_row = Frame::new(
            key(1),
            size,
            Rows::Changed(vec![(0, lines(&["cd"]).pop().unwrap())]),
            None,
            false,
        )
        .unwrap();
        assert_eq!(layout.framed(different_row), Ok(true));
        let cursor = Some(Cursor {
            row: 0,
            column: 1,
            shape: frames::CursorShape::Bar,
            blinking: false,
        });
        let moved_cursor =
            Frame::new(key(1), size, Rows::Changed(Vec::new()), cursor, false).unwrap();
        assert_eq!(layout.framed(moved_cursor), Ok(true));
        let same_cursor =
            Frame::new(key(1), size, Rows::Changed(Vec::new()), cursor, false).unwrap();
        assert_eq!(layout.framed(same_cursor), Ok(false));
    }

    #[test]
    fn events_typed_while_awaiting_a_focus_change_wait_past_stale_layouts_and_reach_the_new_view() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.await_focus(WaitingFor::Focus(at(2)), now);
        layout.keep_unsent(pasted("a"), now);
        layout.keep_unsent(pasted("b"), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "held while awaiting the new view"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(1)), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "an earlier layout leaves them held"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, found(12, Said::Looking(())))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(2)), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "still waits for the new terminal's key"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(2)), now);
        let expected = Some((key(2), VecDeque::from([pasted("a"), pasted("b")])));
        assert_eq!(
            sent(&mut layout, now),
            expected,
            "flushes in order into the new view"
        );
    }

    #[test]
    fn events_typed_while_awaiting_a_refused_or_superseded_focus_change_are_dropped() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.await_focus(WaitingFor::Focus(at(2)), now);
        layout.keep_unsent(pasted("for-new"), now);
        layout.heard_refused(Echoed::Through(at(1)), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "an earlier refusal leaves the wait standing"
        );
        layout.heard_refused(Echoed::Through(at(2)), now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::new())),
            "not leaked to old"
        );
        layout.keep_unsent(pasted("after"), now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::from([pasted("after")])))
        );

        layout.await_focus(WaitingFor::Focus(at(3)), now);
        layout.keep_unsent(pasted("first"), now);
        layout.await_focus(WaitingFor::Focus(at(4)), now);
        layout.keep_unsent(pasted("second"), now);
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(3)), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "superseded reply leaves newer wait standing"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(13, (0, 0, 20, 4), Some(3)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(4)), now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(3), VecDeque::from([pasted("second")])))
        );

        layout.await_focus(WaitingFor::Focus(at(5)), now);
        layout.keep_unsent(pasted("lost-stream"), now);
        layout.stream_lost(now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(3), VecDeque::new())),
            "dropped on stream loss"
        );
    }

    #[test]
    fn an_older_daemon_with_no_heard_number_unblocks_an_awaited_focus_change() {
        let now = Instant::now();
        let at = AskNumber::of(2).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.await_focus(WaitingFor::Focus(at), now);
        layout.keep_unsent(pasted("a"), now);
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Unnumbered, now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::from([pasted("a")])))
        );
    }

    #[test]
    fn a_welcome_view_is_not_closed_while_focus_is_moving_away() {
        use show_response::view::Found as Said;
        let now = Instant::now();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, found(11, Said::Welcome(())))),
            LaidOut::Shown
        );
        assert!(layout.welcomes());
        layout.await_focus(WaitingFor::Focus(AskNumber::of(2).unwrap()), now);
        assert!(
            !layout.welcomes(),
            "keys typed after leaving welcome wait for the next view"
        );
        layout.heard_refused(Echoed::Through(AskNumber::of(2).unwrap()), now);
        assert!(layout.welcomes());
    }

    #[test]
    fn a_crossing_edge_drops_kept_events_while_a_staying_edge_flushes_them() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let step = |n| NonZeroU64::new(n).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );

        layout.await_focus(
            WaitingFor::Step {
                ask: at(2),
                step: step(1),
            },
            now,
        );
        layout.keep_unsent(pasted("crossing"), now);
        let earlier = layout.heard_edged(
            worktree(1),
            Direction::Left,
            step(1),
            Echoed::Through(at(1)),
            now,
        );
        assert_eq!(
            earlier,
            EdgeHeard::Ignored,
            "an earlier heard leaves the wait standing"
        );
        assert_eq!(sent(&mut layout, now), None);
        let crosses = layout.heard_edged(
            worktree(1),
            Direction::Left,
            step(1),
            Echoed::Through(at(2)),
            now,
        );
        assert_eq!(crosses, EdgeHeard::Crosses(Across::Before));
        layout.crossed_edge(now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::new())),
            "dropped on cross"
        );

        layout.await_focus(
            WaitingFor::Step {
                ask: at(3),
                step: step(2),
            },
            now,
        );
        layout.keep_unsent(pasted("staying-right"), now);
        let right = layout.heard_edged(
            worktree(1),
            Direction::Right,
            step(2),
            Echoed::Through(at(3)),
            now,
        );
        assert_eq!(right, EdgeHeard::Crosses(Across::After));
        layout.stayed_at_edge(now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::from([pasted("staying-right")]))),
            "flushed when a right edge has no worktree tab after it",
        );

        layout.await_focus(
            WaitingFor::Step {
                ask: at(4),
                step: step(3),
            },
            now,
        );
        layout.keep_unsent(pasted("staying-up"), now);
        let up = layout.heard_edged(
            worktree(1),
            Direction::Up,
            step(3),
            Echoed::Through(at(4)),
            now,
        );
        assert_eq!(up, EdgeHeard::Stayed);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::from([pasted("staying-up")]))),
            "flushed when vertical focus stayed on the view",
        );
    }

    #[test]
    fn forgetting_a_pending_step_drops_kept_events_and_ignores_its_edge() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let step = NonZeroU64::new(1).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );

        layout.await_focus(WaitingFor::Step { ask: at(2), step }, now);
        layout.keep_unsent(pasted("abandoned"), now);
        layout.forget_step(now);
        let edged = layout.heard_edged(
            worktree(1),
            Direction::Left,
            step,
            Echoed::Through(at(2)),
            now,
        );
        assert_eq!(edged, EdgeHeard::Ignored);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(1), VecDeque::new())),
            "abandoned step input never flushes into the old terminal",
        );
    }

    #[test]
    fn an_older_daemon_edge_for_a_superseded_step_never_unblocks_a_newer_focus_wait() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let step = NonZeroU64::new(1).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );

        layout.await_focus(WaitingFor::Step { ask: at(2), step }, now);
        layout.await_focus(WaitingFor::Focus(at(3)), now);
        layout.keep_unsent(pasted("for-new"), now);
        let stale = layout.heard_edged(worktree(1), Direction::Left, step, Echoed::Unnumbered, now);
        assert_eq!(stale, EdgeHeard::Ignored);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "stale unnumbered edge never leaks into old tab"
        );
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Unnumbered, now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::from([pasted("for-new")])))
        );
    }

    #[test]
    fn following_a_rehomed_terminal_drops_old_events_and_keeps_unreached_wait() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let step = NonZeroU64::new(1).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.await_focus(WaitingFor::Focus(at(2)), now);
        layout.keep_unsent(pasted("before-rehome"), now);
        let rehomed = laid_on(2, view(11, (0, 0, 20, 4), Some(1)));
        assert!(layout.followed(&rehomed, now));
        assert_eq!(layout.laid_out(rehomed), LaidOut::Shown);
        layout.heard_laid(Echoed::Through(at(1)), now);
        assert_eq!(
            layout.awaiting,
            Some(WaitingFor::Focus(at(2))),
            "unreached ask stays awaited"
        );
        layout.keep_unsent(pasted("after-rehome"), now);
        assert_eq!(
            sent(&mut layout, now),
            None,
            "does not leak into rehomed terminal before ask"
        );
        assert_eq!(
            layout.laid_out(laid_on(2, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        layout.heard_laid(Echoed::Through(at(2)), now);
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::from([pasted("after-rehome")]))),
            "drops old worktree input and flushes post-rehome input into the awaited view",
        );

        layout.await_focus(WaitingFor::Step { ask: at(3), step }, now);
        layout.keep_unsent(pasted("before-step-rehome"), now);
        let rehomed = laid_on(3, view(12, (0, 0, 20, 4), Some(2)));
        assert!(layout.followed(&rehomed, now));
        assert_eq!(layout.laid_out(rehomed), LaidOut::Shown);
        layout.heard_laid(Echoed::Through(at(2)), now);
        let edged = layout.heard_edged(
            worktree(3),
            Direction::Left,
            step,
            Echoed::Through(at(3)),
            now,
        );
        assert_eq!(
            edged,
            EdgeHeard::Crosses(Across::Before),
            "pending step still resolves"
        );
        layout.crossed_edge(now);
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())));

        layout.await_focus(WaitingFor::Focus(at(4)), now);
        layout.keep_unsent(pasted("before-reached-rehome"), now);
        let rehomed = laid_on(4, view(12, (0, 0, 20, 4), Some(2)));
        assert!(layout.followed(&rehomed, now));
        assert_eq!(layout.laid_out(rehomed), LaidOut::Shown);
        layout.heard_laid(Echoed::Through(at(4)), now);
        assert!(
            layout.awaiting.is_none(),
            "reached rehome layout clears focus wait"
        );
        assert_eq!(sent(&mut layout, now), Some((key(2), VecDeque::new())));
    }

    #[test]
    fn events_typed_before_an_empty_worktree_opens_its_first_shell_reach_that_shell() {
        let dir = tempfile::tempdir().unwrap();
        let served = dir.path().join("served");
        luned::folder::make(&served).unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(NonZeroU16::new(20).unwrap(), NonZeroU16::new(6).unwrap());
        let greeting = Greeting {
            foreground: None,
            background: None,
        };
        let browser_config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: Colours::default(),
        };
        let mut viewed = Viewed::start(
            1,
            served,
            Machine::Local,
            worktree(1),
            size,
            greeting,
            None,
            browser_config,
            &events,
        );
        viewed.watched_at = Some(Instant::now());
        let at = |n| Echoed::Through(AskNumber::of(n).unwrap());
        let empty = |id, heard| {
            let size = Some(show_request::Size {
                columns: 20,
                rows: 4,
            });
            let said = show_response::Laid {
                worktree: id,
                size,
                views: Vec::new(),
                shown: None,
                version: None,
            };
            Heard::Laid {
                laid: Laid::heard(said).unwrap(),
                heard,
            }
        };
        let shown = |id, v, k, heard| {
            use show_response::laid::Shown as Said;
            let size = Some(show_request::Size {
                columns: 20,
                rows: 4,
            });
            let views = vec![view(v, (0, 0, 20, 4), Some(k))];
            let said = show_response::Laid {
                worktree: id,
                size,
                views,
                shown: Some(Said::Tiled(v)),
                version: None,
            };
            Heard::Laid {
                laid: Laid::heard(said).unwrap(),
                heard,
            }
        };

        assert!(!viewed.paste("start-a"));
        assert!(matches!(viewed.heard(empty(1, at(1))), Heeded::Changed));
        assert!(!viewed.paste("start-b"));
        assert!(matches!(
            viewed.heard(shown(1, 11, 1, at(2))),
            Heeded::Changed
        ));
        assert_eq!(
            sent(&mut viewed.layout, Instant::now()),
            Some((
                key(1),
                VecDeque::from([pasted("start-a"), pasted("start-b")])
            )),
            "startup typeahead survives auto-opening Beside::Alone",
        );

        viewed.go(&Machine::Local, worktree(2));
        assert!(!viewed.paste("go-a"));
        assert!(matches!(viewed.heard(empty(2, at(3))), Heeded::Changed));
        assert!(!viewed.paste("go-b"));
        assert!(matches!(
            viewed.heard(shown(2, 21, 2, at(4))),
            Heeded::Changed
        ));
        assert_eq!(
            sent(&mut viewed.layout, Instant::now()),
            Some((key(2), VecDeque::from([pasted("go-a"), pasted("go-b")]))),
            "go typeahead survives auto-opening Beside::Alone",
        );
    }

    #[test]
    fn reconnecting_show_hides_stale_arrangement_until_fresh_laid_arrives() {
        use show_response::laid::Shown as Said;
        let dir = tempfile::tempdir().unwrap();
        let served = dir.path().join("served");
        luned::folder::make(&served).unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(NonZeroU16::new(20).unwrap(), NonZeroU16::new(6).unwrap());
        let greeting = Greeting {
            foreground: None,
            background: None,
        };
        let browser_config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: Colours::default(),
        };
        let mut viewed = Viewed::start(
            1,
            served,
            Machine::Local,
            worktree(1),
            size,
            greeting,
            None,
            browser_config,
            &events,
        );
        viewed.watched_at = Some(Instant::now());
        assert!(!viewed.live(), "connecting show is not live yet");
        assert_eq!(viewed.arranged(), None);

        let one = vec![view(11, (0, 0, 20, 4), Some(1))];
        assert!(matches!(
            viewed.heard(laid_in(1, one.clone())),
            Heeded::Changed
        ));
        assert!(viewed.live());
        assert_eq!(viewed.arranged(), Some((Arranged::Panes, None)));

        assert!(matches!(
            viewed.heard(Heard::Lost("gone".to_owned())),
            Heeded::Lost(_)
        ));
        assert!(!viewed.live());
        assert_eq!(viewed.arranged(), None);
        assert_eq!(viewed.focused(), Some(terminal(11)));

        viewed.shown();
        assert!(
            !viewed.live(),
            "reconnecting show stays not live until fresh Laid"
        );
        assert_eq!(
            viewed.arranged(),
            None,
            "stale pre-disconnect arrangement stays hidden"
        );
        assert_eq!(viewed.focused(), Some(terminal(11)));

        let tabs = show_response::Laid {
            worktree: 1,
            size: Some(show_request::Size {
                columns: 20,
                rows: 4,
            }),
            views: one,
            shown: Some(Said::Tabbed(11)),
            version: None,
        };
        let fresh = Heard::Laid {
            laid: Laid::heard(tabs).unwrap(),
            heard: Echoed::Through(AskNumber::HELLO),
        };
        assert!(matches!(viewed.heard(fresh), Heeded::Changed));
        assert!(viewed.live());
        assert_eq!(viewed.arranged(), Some((Arranged::Tabs, None)));
    }

    #[test]
    fn leaving_a_worktree_drops_events_kept_for_an_awaited_focus_change() {
        let now = Instant::now();
        let at = |n| AskNumber::of(n).unwrap();
        let mut layout = Layout::new(worktree(1), None);
        assert_eq!(
            layout.laid_out(laid_on(1, view(11, (0, 0, 20, 4), Some(1)))),
            LaidOut::Shown
        );
        layout.await_focus(WaitingFor::Focus(at(2)), now);
        layout.keep_unsent(pasted("before-left"), now);
        assert!(layout.left(worktree(1), now));
        assert_eq!(
            layout.laid_out(laid_on(1, view(12, (0, 0, 20, 4), Some(2)))),
            LaidOut::Shown
        );
        assert_eq!(
            sent(&mut layout, now),
            Some((key(2), VecDeque::new())),
            "events kept before the worktree left never leak into a later layout",
        );
    }

    #[test]
    fn opening_a_view_after_stepping_cancels_the_pending_step_edge_and_disconnect_hides_arrangement()
     {
        let unserved = tempfile::tempdir().unwrap();
        let (events, _heard) = std::sync::mpsc::channel();
        let size = layout_size(NonZeroU16::new(20).unwrap(), NonZeroU16::new(6).unwrap());
        let greeting = Greeting {
            foreground: None,
            background: None,
        };
        let folder = unserved.path().to_owned();
        let browser_config = embedded::Config {
            mode: embedded::Mode::Unsupported,
            cell: CellPixels {
                width: NonZeroU16::new(10).unwrap(),
                height: NonZeroU16::new(20).unwrap(),
            },
            colours: Colours::default(),
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
        let one = vec![view(11, (0, 0, 20, 4), Some(1))];
        assert!(matches!(viewed.heard(laid_in(1, one)), Heeded::Changed));
        assert_eq!(
            viewed.arranged(),
            None,
            "disconnected show claims no live arrangement"
        );
        assert_eq!(viewed.focused(), Some(terminal(11)));
        let now = Instant::now();
        let step = NonZeroU64::new(1).unwrap();
        viewed.layout.await_focus(
            WaitingFor::Step {
                ask: AskNumber::of(2).unwrap(),
                step,
            },
            now,
        );
        viewed
            .layout
            .await_focus(WaitingFor::Focus(AskNumber::of(3).unwrap()), now);
        let edged = Heard::Edged {
            worktree: worktree(1),
            direction: Direction::Left,
            step,
            heard: Echoed::Through(AskNumber::of(2).unwrap()),
        };
        assert!(
            matches!(viewed.heard(edged), Heeded::Quiet),
            "a newer focus ask cancels an earlier step's edge",
        );
        assert!(matches!(
            viewed.heard(Heard::Lost("gone".to_owned())),
            Heeded::Lost(_)
        ));
        assert!(
            viewed.layout.awaiting.is_none(),
            "losing the stream clears any pending wait"
        );
    }
}
