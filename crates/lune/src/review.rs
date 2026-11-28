use std::io;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::sync::Arc;

use base64::Engine as _;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lune_diff::highlight::{SyntaxRole, TokenSpan};
use lune_diff::layout::{LeftRow, RightRow, SplitLine, UnifiedRow};
use lune_diff::patch::{FilePath, PATCH_BYTES_MAX, Patch};
use lune_diff::ribbon::{
    GUTTER_COLS, Rgb, RibbonImage, RibbonPalette, rasterize_cells, rasterize_rgba,
};
use lune_diff::tree::TreeNode;
use lune_diff::viewer::{
    ActiveLayout, LINE_NUMBER_COLS, SPLIT_HARD_MIN_WIDTH, SearchMatches, SelectionExcerpt, Viewer,
    ViewerFocus, find_case_insensitive_range,
};
use lune_diff::words::ByteSpan;
use lune_protocol::diff::ReadDiff;
use lune_protocol::said::Said;
use luned::api::ReadDiffResponse;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::moonlight;
use crate::sidebar::{self, ACCENT};

pub const TAB: &str = "review";

pub const KITTY_BASE_ID: NonZeroU32 = NonZeroU32::new(4700).expect("kitty base id is non-zero");

const SIDEBAR_WIDTH_COLS: u16 = 28;
const SIDEBAR_PANE_COLS_MIN: u16 = 74;
const KITTY_CHUNK_BYTES: usize = 4096;

const _: () = assert!(PATCH_BYTES_MAX == lune_protocol::PATCH_BYTES_MAX);
const _: () = assert!(SIDEBAR_WIDTH_COLS < SIDEBAR_PANE_COLS_MIN);
const _: () = assert!(KITTY_CHUNK_BYTES > 0);
const _: () = assert!(KITTY_CHUNK_BYTES.is_multiple_of(4));

const DEL_BG: Color = Color::Rgb(0x36, 0x22, 0x30);
const DEL_EMPH_BG: Color = Color::Rgb(0x5c, 0x2e, 0x42);
const ADD_BG: Color = Color::Rgb(0x21, 0x33, 0x2e);
const ADD_EMPH_BG: Color = Color::Rgb(0x31, 0x57, 0x46);
const CURSOR_BG: Color = Color::Rgb(0x2b, 0x29, 0x40);
const SELECT_BG: Color = Color::Rgb(0x3d, 0x35, 0x5c);
const SEARCH_BG: Color = Color::Rgb(0x59, 0x4b, 0x28);

const RIBBON_PALETTE: RibbonPalette = RibbonPalette {
    red_fill: Rgb::new(0x4f, 0x29, 0x39),
    green_fill: Rgb::new(0x28, 0x4a, 0x3d),
    red_edge: Rgb::new(0x8c, 0x48, 0x60),
    green_edge: Rgb::new(0x4c, 0x85, 0x6c),
    fill_alpha: 225,
};

#[derive(Clone, Debug)]
pub struct LoadedDiff {
    base: Said,
    head: Said,
    patch: Arc<Patch>,
}

impl LoadedDiff {
    pub fn of_wire(raw: ReadDiffResponse) -> Result<LoadedDiff, String> {
        let diff = ReadDiff::of_api(raw)?;
        Ok(LoadedDiff::of_diff(&diff))
    }

    #[must_use]
    pub fn of_diff(diff: &ReadDiff) -> LoadedDiff {
        let mut patch = Patch::parse(diff.patch());
        patch.truncated |= diff.truncated();
        for file in diff.untracked() {
            if let Some(path) = FilePath::parse(file.path().as_str()) {
                patch.push_untracked(path, file.content());
            }
        }
        LoadedDiff {
            base: diff.base().clone(),
            head: diff.head().clone(),
            patch: Arc::new(patch),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    None,
    Reload,
    Explain(SelectionExcerpt),
    Ask(SelectionExcerpt),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RibbonRect {
    pub x: u16,
    pub y: u16,
    pub width: NonZeroU16,
    pub height: NonZeroU16,
}

impl RibbonRect {
    #[must_use]
    pub fn from_rect(rect: Rect) -> Option<RibbonRect> {
        Some(RibbonRect {
            x: rect.x,
            y: rect.y,
            width: NonZeroU16::new(rect.width)?,
            height: NonZeroU16::new(rect.height)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyTarget {
    pub cell_px: (NonZeroU16, NonZeroU16),
    pub image_id: NonZeroU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RibbonCacheKey {
    pub rect: RibbonRect,
    pub scroll: usize,
    pub revision: NonZeroU64,
    pub cell_px: (NonZeroU16, NonZeroU16),
}

#[derive(Clone, Debug)]
pub struct RibbonOverlay {
    pub image_id: NonZeroU32,
    pub rect: RibbonRect,
    pub key: RibbonCacheKey,
    pub image: Arc<RibbonImage>,
}

#[derive(Debug)]
enum PaneState {
    Loading,
    Failed {
        why: String,
        revision: NonZeroU64,
    },
    Loaded {
        viewer: Box<Viewer>,
        revision: NonZeroU64,
        stale: Option<String>,
    },
}

#[derive(Debug)]
pub struct ReviewPane {
    state: PaneState,
    last_area: Rect,
    tree_scroll: usize,
    cached_ribbon: Option<(RibbonCacheKey, Option<Arc<RibbonImage>>)>,
}

impl ReviewPane {
    #[must_use]
    pub fn loading() -> ReviewPane {
        ReviewPane {
            state: PaneState::Loading,
            last_area: Rect::default(),
            tree_scroll: 0,
            cached_ribbon: None,
        }
    }

    #[must_use]
    pub fn loaded(
        &mut self,
        response: Result<Arc<LoadedDiff>, String>,
        revision: NonZeroU64,
    ) -> bool {
        let current_revision = match &self.state {
            PaneState::Loading => None,
            PaneState::Failed { revision, .. } | PaneState::Loaded { revision, .. } => {
                Some(*revision)
            }
        };
        if current_revision.is_some_and(|current| revision < current) {
            return false;
        }
        match response {
            Ok(diff) => {
                self.cached_ribbon = None;
                match &mut self.state {
                    PaneState::Loaded {
                        viewer,
                        revision: current_revision,
                        stale,
                    } => {
                        viewer.reload(
                            Arc::clone(&diff.patch),
                            diff.base.as_str().to_owned(),
                            diff.head.as_str().to_owned(),
                        );
                        *current_revision = revision;
                        *stale = None;
                    }
                    PaneState::Loading | PaneState::Failed { .. } => {
                        let viewer = Box::new(Viewer::new(
                            Arc::clone(&diff.patch),
                            diff.base.as_str().to_owned(),
                            diff.head.as_str().to_owned(),
                        ));
                        self.state = PaneState::Loaded {
                            viewer,
                            revision,
                            stale: None,
                        };
                    }
                }
                if !self.last_area.is_empty() {
                    let _ = self.sync_geometry(self.last_area);
                }
            }
            Err(why) => match &mut self.state {
                PaneState::Loaded {
                    revision: current_revision,
                    stale,
                    ..
                } => {
                    *current_revision = revision;
                    *stale = Some(why);
                }
                PaneState::Loading | PaneState::Failed { .. } => {
                    self.state = PaneState::Failed { why, revision };
                }
            },
        }
        true
    }

    #[must_use]
    pub fn tab_label(&self) -> String {
        match &self.state {
            PaneState::Loaded { viewer, .. }
                if viewer.patch().added > 0 || viewer.patch().deleted > 0 =>
            {
                format!(
                    "{TAB} +{} -{}",
                    viewer.patch().added,
                    viewer.patch().deleted
                )
            }
            _ => TAB.to_owned(),
        }
    }

    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self.state, PaneState::Loading)
    }

    #[must_use]
    pub fn is_searching(&self) -> bool {
        matches!(
            &self.state,
            PaneState::Loaded { viewer, .. } if viewer.focus().is_search_editing()
        )
    }

    pub fn key(&mut self, key: &KeyEvent) -> ReviewAction {
        if key.kind == KeyEventKind::Release {
            return ReviewAction::None;
        }
        let (_, body, can_show_tree) = if self.last_area.is_empty() {
            (Rect::default(), Rect::default(), false)
        } else {
            self.sync_geometry(self.last_area)
        };
        if self.is_searching() {
            return self.search_key(key, body);
        }
        let bare = key.modifiers.is_empty();
        let shift = key.modifiers == KeyModifiers::SHIFT;
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        if bare && key.code == KeyCode::Char('r') {
            return ReviewAction::Reload;
        }
        let viewport = usize::from(body.height).max(1);
        let half_page = viewport.div_ceil(2);
        let last_area = self.last_area;
        let PaneState::Loaded { viewer, .. } = &mut self.state else {
            return ReviewAction::None;
        };
        let mut geometry_changed = false;
        match viewer.focus() {
            ViewerFocus::Tree => match key.code {
                KeyCode::Char('j') | KeyCode::Down if bare => viewer.move_down(1, viewport),
                KeyCode::Char('k') | KeyCode::Up if bare => viewer.move_up(1, viewport),
                KeyCode::Char('d') if ctrl => viewer.move_down(half_page, viewport),
                KeyCode::Char('u') if ctrl => viewer.move_up(half_page, viewport),
                KeyCode::PageDown if bare => viewer.move_down(viewport, viewport),
                KeyCode::PageUp if bare => viewer.move_up(viewport, viewport),
                KeyCode::Char('g') | KeyCode::Home if bare => viewer.top(viewport),
                KeyCode::Char('G') if shift || bare => viewer.bottom(viewport),
                KeyCode::End if bare => viewer.bottom(viewport),
                KeyCode::Char('f') if bare => {
                    viewer.toggle_sidebar();
                    geometry_changed = true;
                }
                KeyCode::Tab if bare => {
                    viewer.toggle_focus(can_show_tree);
                    geometry_changed = true;
                }
                KeyCode::Char('m') if bare => viewer.toggle_viewed(),
                KeyCode::Enter if bare => viewer.activate_tree_entry(viewport),
                KeyCode::Esc if bare => viewer.unfocus_sidebar(),
                _ => {}
            },
            ViewerFocus::Diff { .. } => match key.code {
                KeyCode::Char('j') | KeyCode::Down if bare => viewer.move_down(1, viewport),
                KeyCode::Char('k') | KeyCode::Up if bare => viewer.move_up(1, viewport),
                KeyCode::Char('d') if ctrl => viewer.move_down(half_page, viewport),
                KeyCode::Char('u') if ctrl => viewer.move_up(half_page, viewport),
                KeyCode::PageDown if bare => viewer.move_down(viewport, viewport),
                KeyCode::PageUp if bare => viewer.move_up(viewport, viewport),
                KeyCode::Char('g') | KeyCode::Home if bare => viewer.top(viewport),
                KeyCode::Char('G') if shift || bare => viewer.bottom(viewport),
                KeyCode::End if bare => viewer.bottom(viewport),
                KeyCode::Char(']') if bare => viewer.next_hunk(viewport),
                KeyCode::Char('[') if bare => viewer.prev_hunk(viewport),
                KeyCode::Char('}') if shift || bare => viewer.next_file(viewport),
                KeyCode::Char('{') if shift || bare => viewer.prev_file(viewport),
                KeyCode::Char('s') if bare => viewer.toggle_layout(body.width),
                KeyCode::Char('f') if bare => {
                    viewer.toggle_sidebar();
                    geometry_changed = true;
                }
                KeyCode::Tab if bare => {
                    viewer.toggle_focus(can_show_tree);
                    geometry_changed = true;
                }
                KeyCode::Char('m') if bare => viewer.toggle_viewed(),
                KeyCode::Char('v') if bare => viewer.toggle_select(),
                KeyCode::Char('/') if bare => viewer.start_search(),
                KeyCode::Char('n') if bare => viewer.search_next(viewport),
                KeyCode::Char('N') if shift || bare => viewer.search_prev(viewport),
                KeyCode::Esc if bare => {
                    if viewer.is_selecting() {
                        viewer.clear_select();
                    } else if viewer.committed_search().is_some() {
                        viewer.search_cancel();
                    }
                }
                KeyCode::Char('e') if bare => {
                    if let Some(excerpt) = viewer.selected_excerpt() {
                        return ReviewAction::Explain(excerpt);
                    }
                }
                KeyCode::Char('?') if shift || bare => {
                    if let Some(excerpt) = viewer.selected_excerpt() {
                        return ReviewAction::Ask(excerpt);
                    }
                }
                _ => {}
            },
            ViewerFocus::SearchEditing { .. } => {
                unreachable!("search editing is handled before mode dispatch")
            }
        }
        if geometry_changed && !last_area.is_empty() {
            let _ = self.sync_geometry(last_area);
        }
        ReviewAction::None
    }

    fn search_key(&mut self, key: &KeyEvent, body: Rect) -> ReviewAction {
        let viewport = usize::from(body.height).max(1);
        let PaneState::Loaded { viewer, .. } = &mut self.state else {
            return ReviewAction::None;
        };
        let bare = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        match key.code {
            KeyCode::Esc => viewer.search_cancel(),
            KeyCode::Enter => viewer.search_commit(),
            KeyCode::Backspace => viewer.search_pop(viewport),
            KeyCode::Char(ch) if bare => viewer.search_push(ch, viewport),
            _ => {}
        }
        ReviewAction::None
    }

    pub fn mouse(&mut self, mouse: MouseEvent, local: Position) {
        let area = self.last_area;
        if area.is_empty() {
            return;
        }
        let (tree_area, diff_area, _) = self.sync_geometry(area);
        let viewport = usize::from(diff_area.height).max(1);
        let PaneState::Loaded { viewer, .. } = &mut self.state else {
            return;
        };
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if !tree_area.is_empty() && local.x < tree_area.width {
                    viewer.scroll_tree_by(3);
                } else {
                    viewer.scroll_by(3, viewport);
                }
            }
            MouseEventKind::ScrollUp => {
                if !tree_area.is_empty() && local.x < tree_area.width {
                    viewer.scroll_tree_by(-3);
                } else {
                    viewer.scroll_by(-3, viewport);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if local.y > 0 && local.y + 1 < area.height => {
                let body_row = usize::from(local.y - 1);
                if !tree_area.is_empty() && local.x < tree_area.width {
                    let index = self.tree_scroll.saturating_add(body_row);
                    viewer.activate_tree_index(index, viewport);
                } else {
                    let index = viewer.scroll().saturating_add(body_row);
                    viewer.click_diff_row(index, viewport);
                }
            }
            _ => {}
        }
    }

    pub fn draw(
        &mut self,
        buffer: &mut Buffer,
        area: Rect,
        focused: bool,
        kitty: Option<KittyTarget>,
    ) -> Option<RibbonOverlay> {
        self.last_area = area;
        if area.is_empty() {
            return None;
        }
        let header = Rect::new(area.x, area.y, area.width, 1);
        let footer_y = area.bottom().saturating_sub(1);
        let has_footer = area.height >= 2;
        let footer = if has_footer {
            Rect::new(area.x, footer_y, area.width, 1)
        } else {
            Rect::default()
        };
        let (tree_area, diff_area, _) = self.sync_geometry(area);

        let overlay = match &mut self.state {
            PaneState::Loading => {
                draw_banner(
                    buffer,
                    header,
                    " review · loading changes…",
                    moonlight::MUTE,
                );
                if !diff_area.is_empty() {
                    let message = " reading worktree changes…";
                    buffer.set_stringn(
                        diff_area.x,
                        diff_area.y + 1.min(diff_area.height.saturating_sub(1)),
                        message,
                        usize::from(diff_area.width),
                        Style::new().fg(moonlight::MUTE),
                    );
                }
                None
            }
            PaneState::Failed { why, .. } => {
                draw_banner(
                    buffer,
                    header,
                    " review · failed to read changes",
                    moonlight::RED,
                );
                if !diff_area.is_empty() {
                    let message = format!(" {why}");
                    buffer.set_stringn(
                        diff_area.x,
                        diff_area.y + 1.min(diff_area.height.saturating_sub(1)),
                        &message,
                        usize::from(diff_area.width),
                        Style::new().fg(moonlight::RED),
                    );
                }
                None
            }
            PaneState::Loaded {
                viewer,
                revision,
                stale,
            } => {
                let viewport = usize::from(diff_area.height).max(1);
                viewer.keep_visible(viewport);
                draw_header(buffer, header, viewer, stale.as_deref());
                if !tree_area.is_empty() {
                    let tree_height = usize::from(tree_area.height).max(1);
                    if viewer.tree_cursor() < self.tree_scroll {
                        self.tree_scroll = viewer.tree_cursor();
                    } else if viewer.tree_cursor() >= self.tree_scroll + tree_height {
                        self.tree_scroll = viewer.tree_cursor() + 1 - tree_height;
                    }
                    draw_tree(buffer, tree_area, viewer, self.tree_scroll, focused);
                    let separator_x = tree_area.right();
                    if separator_x < area.right() {
                        let separator_style = Style::new().fg(moonlight::LIFTED);
                        for y in tree_area.top()..tree_area.bottom() {
                            buffer.set_string(separator_x, y, "│", separator_style);
                        }
                    }
                }
                if viewer.patch().files.is_empty() {
                    if !diff_area.is_empty() {
                        let message = format!(
                            " No changes between {} and {}.",
                            viewer.base(),
                            viewer.head()
                        );
                        buffer.set_stringn(
                            diff_area.x,
                            diff_area.y + 1.min(diff_area.height.saturating_sub(1)),
                            &message,
                            usize::from(diff_area.width),
                            Style::new().fg(moonlight::MUTE),
                        );
                    }
                    None
                } else {
                    match viewer.active_layout() {
                        ActiveLayout::Split => draw_split(
                            buffer,
                            diff_area,
                            viewer,
                            *revision,
                            focused,
                            kitty,
                            &mut self.cached_ribbon,
                        ),
                        ActiveLayout::Unified => {
                            draw_unified(buffer, diff_area, viewer, focused);
                            None
                        }
                    }
                }
            }
        };
        if has_footer {
            draw_hints(buffer, footer, HintMode::of(&self.state));
        }
        overlay
    }

    fn sync_geometry(&mut self, area: Rect) -> (Rect, Rect, bool) {
        let can_show_tree = match &self.state {
            PaneState::Loaded { viewer, .. } => {
                area.width >= SIDEBAR_PANE_COLS_MIN && !viewer.tree().entries.is_empty()
            }
            PaneState::Loading | PaneState::Failed { .. } => false,
        };
        if let PaneState::Loaded { viewer, .. } = &mut self.state {
            viewer.sync_tree_availability(can_show_tree);
        }
        let (tree_area, diff_area) = self.split_areas(area);
        if let PaneState::Loaded { viewer, .. } = &mut self.state {
            viewer.sync_layout(diff_area.width);
        }
        (tree_area, diff_area, can_show_tree)
    }

    fn split_areas(&self, area: Rect) -> (Rect, Rect) {
        let body_y = area.y.saturating_add(1).min(area.bottom());
        let body_height = area.height.saturating_sub(2);
        let body = Rect::new(area.x, body_y, area.width, body_height);
        let show_tree = matches!(
            &self.state,
            PaneState::Loaded { viewer, .. }
                if viewer.is_sidebar_open() && !viewer.tree().entries.is_empty()
        ) && area.width >= SIDEBAR_PANE_COLS_MIN;
        let (tree, diff) = if show_tree {
            let tree_width = SIDEBAR_WIDTH_COLS.min(area.width / 3);
            let tree = Rect::new(body.x, body.y, tree_width, body.height);
            let diff_x = body.x.saturating_add(tree_width).saturating_add(1);
            let diff_width = area.right().saturating_sub(diff_x);
            (tree, Rect::new(diff_x, body.y, diff_width, body.height))
        } else {
            (Rect::default(), body)
        };
        assert!(tree.right() <= area.right(), "tree area fits pane width");
        assert!(diff.right() <= area.right(), "diff area fits pane width");
        assert!(diff.bottom() <= area.bottom(), "diff area fits pane height");
        (tree, diff)
    }
}

fn draw_banner(buffer: &mut Buffer, area: Rect, text: &str, color: Color) {
    let style = Style::new().fg(color).bg(moonlight::SURFACE);
    fill_row(buffer, area, style);
    buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
}

fn draw_header(buffer: &mut Buffer, area: Rect, viewer: &Viewer, stale: Option<&str>) {
    let bg = moonlight::SURFACE;
    fill_row(buffer, area, Style::new().bg(bg));
    let mut spans = vec![
        Span::styled(
            " review",
            Style::new().fg(ACCENT).bg(bg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {} → {}", viewer.base(), viewer.head()),
            Style::new().fg(moonlight::TEXT).bg(bg),
        ),
    ];
    if viewer.patch().added > 0 || viewer.patch().deleted > 0 {
        spans.push(Span::styled("  ", Style::new().bg(bg)));
        spans.push(Span::styled(
            format!("+{}", viewer.patch().added),
            Style::new()
                .fg(moonlight::GREEN)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", Style::new().bg(bg)));
        spans.push(Span::styled(
            format!("-{}", viewer.patch().deleted),
            Style::new()
                .fg(moonlight::RED)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let files = viewer.patch().files.len();
    let seen = viewer.viewed_count();
    let mode = match viewer.active_layout() {
        ActiveLayout::Split => "split",
        ActiveLayout::Unified => "unified",
    };
    spans.push(Span::styled(
        format!("  · {files} files · {seen}/{files} ✓ · [{mode}]"),
        Style::new().fg(moonlight::MUTE).bg(bg),
    ));
    if viewer.patch().truncated {
        spans.push(Span::styled(
            " [truncated]",
            Style::new().fg(moonlight::YELLOW).bg(bg),
        ));
    }
    if let Some(why) = stale {
        spans.push(Span::styled(
            format!(" · stale: {why}"),
            Style::new().fg(moonlight::RED).bg(bg),
        ));
    }
    let line = Line::from(spans);
    buffer.set_line(area.x, area.y, &line, area.width);
}

#[derive(Clone, Copy)]
enum HintMode<'a> {
    Loading,
    Failed,
    SearchEditing {
        query: &'a str,
        found: Option<&'a SearchMatches>,
    },
    Tree,
    Visual,
    SearchCommitted {
        query: &'a str,
        found: Option<&'a SearchMatches>,
    },
    Normal,
}

impl<'a> HintMode<'a> {
    fn of(state: &'a PaneState) -> HintMode<'a> {
        match state {
            PaneState::Loading => HintMode::Loading,
            PaneState::Failed { .. } => HintMode::Failed,
            PaneState::Loaded { viewer, .. } => match viewer.focus() {
                ViewerFocus::SearchEditing { query, found } => HintMode::SearchEditing {
                    query: query.as_str(),
                    found: found.as_ref(),
                },
                ViewerFocus::Tree => HintMode::Tree,
                ViewerFocus::Diff {
                    selection_anchor: Some(_),
                } => HintMode::Visual,
                ViewerFocus::Diff {
                    selection_anchor: None,
                } => match viewer.committed_search() {
                    Some(committed) => HintMode::SearchCommitted {
                        query: committed.query(),
                        found: committed.found(),
                    },
                    None => HintMode::Normal,
                },
            },
        }
    }
}

fn draw_hints(buffer: &mut Buffer, area: Rect, mode: HintMode<'_>) {
    assert_eq!(area.height, 1, "review hints bar occupies one row");
    let bg = moonlight::SURFACE;
    fill_row(buffer, area, Style::new().bg(bg));
    let (text, fg) = match mode {
        HintMode::SearchEditing { query, found } => {
            draw_search_hint(buffer, area, bg, query, "_", found);
            return;
        }
        HintMode::SearchCommitted { query, found } => {
            draw_search_hint(buffer, area, bg, query, "", found);
            return;
        }
        HintMode::Loading | HintMode::Failed => (" r reload · dd close", moonlight::MUTE),
        HintMode::Tree => (
            " FILES · j/k move · Enter open/fold · m viewed · Tab diff · Esc back",
            ACCENT,
        ),
        HintMode::Visual => (
            " VISUAL · j/k extend · e explain · ? ask · Esc clear",
            moonlight::MAUVE,
        ),
        HintMode::Normal => (
            " j/k move · ]/[ hunk · }/{ file · v select · e explain · ? ask · s split · f files · m viewed · / search · dd close",
            moonlight::MUTE,
        ),
    };
    assert!(!text.is_empty(), "review hints text is non-empty");
    buffer.set_stringn(
        area.x,
        area.y,
        text,
        usize::from(area.width),
        Style::new().fg(fg).bg(bg),
    );
}

fn draw_search_hint(
    buffer: &mut Buffer,
    area: Rect,
    bg: Color,
    query: &str,
    cursor: &str,
    found: Option<&SearchMatches>,
) {
    let count = match found {
        None => "0 matches".to_owned(),
        Some(matches) => format!("{}/{}", matches.current_index() + 1, matches.count()),
    };
    let line = Line::from(vec![
        Span::styled(
            " /",
            Style::new()
                .fg(moonlight::YELLOW)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{query}{cursor}"),
            Style::new().fg(moonlight::TEXT).bg(bg),
        ),
        Span::styled(
            format!("  ({count})"),
            Style::new().fg(moonlight::MUTE).bg(bg),
        ),
    ]);
    buffer.set_line(area.x, area.y, &line, area.width);
}

fn draw_tree(buffer: &mut Buffer, area: Rect, viewer: &Viewer, scroll: usize, focused: bool) {
    let width = usize::from(area.width);
    let tree_focused = focused && viewer.is_tree_focused();
    for (offset, y) in (area.top()..area.bottom()).enumerate() {
        let index = scroll.saturating_add(offset);
        let Some(entry) = viewer.tree().entries.get(index) else {
            break;
        };
        let active = index == viewer.tree_cursor();
        let bg = if active && tree_focused {
            moonlight::LIFTED
        } else if active {
            moonlight::SURFACE
        } else {
            Color::Reset
        };
        if bg != Color::Reset {
            fill_row(
                buffer,
                Rect::new(area.x, y, area.width, 1),
                Style::new().bg(bg),
            );
        }
        let indent = " ".repeat((entry.depth * 2).min(width / 2));
        match &entry.node {
            TreeNode::Dir {
                name,
                path,
                added,
                deleted,
            } => {
                let stats = format!(" +{added} -{deleted}");
                let chevron = if viewer.is_folded(path) { "▸" } else { "▾" };
                let lead = format!(" {indent}{chevron} ");
                let room = width.saturating_sub(lead.width() + stats.width() + 1);
                let shown = sidebar::cut(name, room);
                let pad = width.saturating_sub(lead.width() + shown.width() + stats.width());
                let line = Line::from(vec![
                    Span::styled(lead, Style::new().fg(moonlight::MUTE).bg(bg)),
                    Span::styled(shown, Style::new().fg(moonlight::BLUE).bg(bg)),
                    Span::styled(" ".repeat(pad), Style::new().bg(bg)),
                    Span::styled(
                        format!(" +{added}"),
                        Style::new().fg(moonlight::GREEN).bg(bg),
                    ),
                    Span::styled(
                        format!(" -{deleted}"),
                        Style::new().fg(moonlight::RED).bg(bg),
                    ),
                ]);
                buffer.set_line(area.x, y, &line, area.width);
            }
            TreeNode::File {
                file,
                name,
                status,
                added,
                deleted,
            } => {
                let seen = viewer.is_viewed(*file);
                let (badge, badge_color) = if seen {
                    ("✓", moonlight::GREEN)
                } else {
                    let color = match status {
                        lune_diff::FileStatus::Added => moonlight::GREEN,
                        lune_diff::FileStatus::Deleted => moonlight::RED,
                        lune_diff::FileStatus::Renamed => moonlight::CYAN,
                        lune_diff::FileStatus::Modified => moonlight::YELLOW,
                    };
                    (status.badge(), color)
                };
                let stats = format!(" +{added} -{deleted}");
                let lead = format!(" {indent}{badge} ");
                let room = width.saturating_sub(lead.width() + stats.width() + 1);
                let shown = sidebar::cut(name, room);
                let pad = width.saturating_sub(lead.width() + shown.width() + stats.width());
                let name_fg = if seen {
                    moonlight::MUTE
                } else {
                    moonlight::TEXT
                };
                let line = Line::from(vec![
                    Span::styled(lead, Style::new().fg(badge_color).bg(bg)),
                    Span::styled(shown, Style::new().fg(name_fg).bg(bg)),
                    Span::styled(" ".repeat(pad), Style::new().bg(bg)),
                    Span::styled(
                        format!(" +{added}"),
                        Style::new().fg(moonlight::GREEN).bg(bg),
                    ),
                    Span::styled(
                        format!(" -{deleted}"),
                        Style::new().fg(moonlight::RED).bg(bg),
                    ),
                ]);
                buffer.set_line(area.x, y, &line, area.width);
            }
        }
    }
}

fn draw_split(
    buffer: &mut Buffer,
    area: Rect,
    viewer: &Viewer,
    revision: NonZeroU64,
    focused: bool,
    kitty: Option<KittyTarget>,
    cached_ribbon: &mut Option<(RibbonCacheKey, Option<Arc<RibbonImage>>)>,
) -> Option<RibbonOverlay> {
    if area.is_empty() {
        return None;
    }
    let center_width = LINE_NUMBER_COLS + 1 + GUTTER_COLS + 1 + LINE_NUMBER_COLS;
    assert!(
        area.width >= SPLIT_HARD_MIN_WIDTH,
        "ActiveLayout::Split requires diff width >= SPLIT_HARD_MIN_WIDTH"
    );
    let code_total = area.width - center_width;
    let left_width = code_total / 2;
    let right_width = code_total - left_width;
    let left_x = area.x;
    let old_number_x = left_x + left_width;
    let gutter_x = old_number_x + LINE_NUMBER_COLS + 1;
    let new_number_x = gutter_x + GUTTER_COLS + 1;
    let right_x = new_number_x + LINE_NUMBER_COLS;
    assert!(
        left_width > 0 && right_width > 0,
        "both split code columns are positive"
    );
    assert_eq!(
        right_x + right_width,
        area.right(),
        "split columns tile diff area"
    );

    let visible_rows = usize::from(area.height);
    let selection = viewer.selection_range();
    let search_query = viewer.active_search_query();
    let fallback_cells = if kitty.is_some() {
        None
    } else {
        Some(rasterize_cells(
            &viewer.split().ribbons,
            viewer.scroll(),
            visible_rows,
            GUTTER_COLS,
            RIBBON_PALETTE,
        ))
    };

    for offset in 0..visible_rows {
        let row_index = viewer.scroll() + offset;
        let Some(row) = viewer.split().rows.get(row_index) else {
            break;
        };
        let y_offset = u16::try_from(offset).expect("visible row offset fits u16");
        let y = area.y + y_offset;
        let is_cursor = focused && !viewer.is_tree_focused() && row_index == viewer.cursor();
        let is_selected = selection
            .as_ref()
            .is_some_and(|range| range.contains(&row_index));

        match row {
            SplitLine::FileHeader { file } => {
                draw_file_banner(
                    buffer,
                    Rect::new(area.x, y, area.width, 1),
                    viewer,
                    *file,
                    is_cursor,
                );
            }
            SplitLine::BinaryNotice { .. } => {
                buffer.set_stringn(
                    left_x,
                    y,
                    "Binary file changed",
                    usize::from(left_width),
                    Style::new().fg(moonlight::YELLOW),
                );
                buffer.set_stringn(
                    right_x,
                    y,
                    "Binary file changed",
                    usize::from(right_width),
                    Style::new().fg(moonlight::YELLOW),
                );
            }
            SplitLine::Pair(pair) => {
                if let Some(left) = pair.left() {
                    draw_left_row(
                        buffer,
                        Rect::new(left_x, y, left_width, 1),
                        old_number_x,
                        left,
                        viewer,
                        is_cursor,
                        is_selected,
                        search_query,
                    );
                }
                if let Some(right) = pair.right() {
                    draw_right_row(
                        buffer,
                        Rect::new(right_x, y, right_width, 1),
                        new_number_x,
                        right,
                        viewer,
                        is_cursor,
                        is_selected,
                        search_query,
                    );
                }
                if let Some(grid) = &fallback_cells {
                    for (column, cell) in grid[offset].iter().enumerate() {
                        let col_offset =
                            u16::try_from(column).expect("gutter column offset fits u16");
                        let x = gutter_x + col_offset;
                        if x < area.right() {
                            let mut style = Style::new();
                            if let Some(fg) = cell.fg {
                                style = style.fg(Color::Rgb(fg.r, fg.g, fg.b));
                            }
                            if let Some(bg) = cell.bg {
                                style = style.bg(Color::Rgb(bg.r, bg.g, bg.b));
                            }
                            let mut utf8 = [0u8; 4];
                            buffer.set_string(x, y, cell.glyph.encode_utf8(&mut utf8), style);
                        }
                    }
                }
            }
        }
    }

    let kitty = kitty?;
    let gutter_rect = RibbonRect::from_rect(Rect::new(gutter_x, area.y, GUTTER_COLS, area.height))?;
    let key = RibbonCacheKey {
        rect: gutter_rect,
        scroll: viewer.scroll(),
        revision,
        cell_px: kitty.cell_px,
    };
    let image = match cached_ribbon {
        Some((cached_key, cached_image)) if *cached_key == key => cached_image.clone(),
        _ => {
            let built = rasterize_rgba(
                &viewer.split().ribbons,
                viewer.scroll(),
                visible_rows,
                GUTTER_COLS,
                kitty.cell_px,
                RIBBON_PALETTE,
            )
            .map(Arc::new);
            *cached_ribbon = Some((key, built.clone()));
            built
        }
    }?;
    Some(RibbonOverlay {
        image_id: kitty.image_id,
        rect: gutter_rect,
        key,
        image,
    })
}

fn draw_line_number_and_bg(
    buffer: &mut Buffer,
    code_rect: Rect,
    number_x: u16,
    base_bg: Color,
    number_fg: Color,
    line_number: Option<u32>,
    is_cursor: bool,
) {
    if base_bg != Color::Reset {
        fill_row(buffer, code_rect, Style::new().bg(base_bg));
        fill_row(
            buffer,
            Rect::new(number_x, code_rect.y, LINE_NUMBER_COLS, 1),
            Style::new().bg(base_bg),
        );
    }
    if let Some(number) = line_number {
        let number_text = format_line_number(number);
        let mut style = Style::new().fg(number_fg).bg(base_bg);
        if is_cursor {
            style = style.add_modifier(Modifier::BOLD);
        }
        buffer.set_stringn(
            number_x,
            code_rect.y,
            &number_text,
            usize::from(LINE_NUMBER_COLS),
            style,
        );
    }
}

fn format_line_number(number: u32) -> String {
    let cols = usize::from(LINE_NUMBER_COLS);
    if number < 100_000 {
        format!("{number:>cols$}")
    } else {
        let tail = number % 10_000;
        format!("…{tail:04}")
    }
}

fn blank_line_number() -> String {
    " ".repeat(usize::from(LINE_NUMBER_COLS))
}

fn draw_left_row(
    buffer: &mut Buffer,
    code_rect: Rect,
    number_x: u16,
    row: &LeftRow,
    viewer: &Viewer,
    is_cursor: bool,
    is_selected: bool,
    search_query: Option<&str>,
) {
    match row {
        LeftRow::Deleted { line, at } => {
            let base_bg = row_bg(DEL_BG, is_cursor, is_selected);
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::RED,
                Some(*line),
                is_cursor,
            );
            let entry = at.get(viewer.patch());
            draw_styled_code(
                buffer,
                code_rect,
                &entry.text,
                &entry.syntax,
                &entry.emphasis,
                base_bg,
                Some(DEL_EMPH_BG),
                search_query,
            );
        }
        LeftRow::Context { line, at } => {
            let base_bg = row_bg(Color::Reset, is_cursor, is_selected);
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::MUTE,
                Some(*line),
                is_cursor,
            );
            let entry = at.get(viewer.patch());
            draw_styled_code(
                buffer,
                code_rect,
                &entry.text,
                &entry.syntax,
                &[],
                base_bg,
                None,
                search_query,
            );
        }
        LeftRow::HunkHeader { text, .. } => {
            let base_bg = moonlight::SURFACE;
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::CYAN,
                None,
                is_cursor,
            );
            let shown = sidebar::cut(text, usize::from(code_rect.width));
            buffer.set_stringn(
                code_rect.x,
                code_rect.y,
                &shown,
                usize::from(code_rect.width),
                Style::new().fg(moonlight::CYAN).bg(base_bg),
            );
        }
    }
}

fn draw_right_row(
    buffer: &mut Buffer,
    code_rect: Rect,
    number_x: u16,
    row: &RightRow,
    viewer: &Viewer,
    is_cursor: bool,
    is_selected: bool,
    search_query: Option<&str>,
) {
    match row {
        RightRow::Added { line, at } => {
            let base_bg = row_bg(ADD_BG, is_cursor, is_selected);
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::GREEN,
                Some(*line),
                is_cursor,
            );
            let entry = at.get(viewer.patch());
            draw_styled_code(
                buffer,
                code_rect,
                &entry.text,
                &entry.syntax,
                &entry.emphasis,
                base_bg,
                Some(ADD_EMPH_BG),
                search_query,
            );
        }
        RightRow::Context { line, at } => {
            let base_bg = row_bg(Color::Reset, is_cursor, is_selected);
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::MUTE,
                Some(*line),
                is_cursor,
            );
            let entry = at.get(viewer.patch());
            draw_styled_code(
                buffer,
                code_rect,
                &entry.text,
                &entry.syntax,
                &[],
                base_bg,
                None,
                search_query,
            );
        }
        RightRow::HunkHeader { text, .. } => {
            let base_bg = moonlight::SURFACE;
            draw_line_number_and_bg(
                buffer,
                code_rect,
                number_x,
                base_bg,
                moonlight::CYAN,
                None,
                is_cursor,
            );
            let shown = sidebar::cut(text, usize::from(code_rect.width));
            buffer.set_stringn(
                code_rect.x,
                code_rect.y,
                &shown,
                usize::from(code_rect.width),
                Style::new().fg(moonlight::CYAN).bg(base_bg),
            );
        }
    }
}

fn draw_unified(buffer: &mut Buffer, area: Rect, viewer: &Viewer, focused: bool) {
    let visible_rows = usize::from(area.height);
    let selection = viewer.selection_range();
    let search_query = viewer.active_search_query();
    let prefix_width = LINE_NUMBER_COLS + 1 + LINE_NUMBER_COLS + 3;

    for offset in 0..visible_rows {
        let row_index = viewer.scroll() + offset;
        let Some(row) = viewer.unified().rows.get(row_index) else {
            break;
        };
        let y_offset = u16::try_from(offset).expect("visible row offset fits u16");
        let y = area.y + y_offset;
        let is_cursor = focused && !viewer.is_tree_focused() && row_index == viewer.cursor();
        let is_selected = selection
            .as_ref()
            .is_some_and(|range| range.contains(&row_index));

        let (old_text, new_text, marker, marker_fg, base_bg, emphasis_bg, text, syntax, emphasis) =
            match row {
                UnifiedRow::FileHeader { file } => {
                    draw_file_banner(
                        buffer,
                        Rect::new(area.x, y, area.width, 1),
                        viewer,
                        *file,
                        is_cursor,
                    );
                    continue;
                }
                UnifiedRow::HunkHeader { text, .. } => {
                    let bg = if is_cursor {
                        moonlight::LIFTED
                    } else {
                        moonlight::SURFACE
                    };
                    fill_row(
                        buffer,
                        Rect::new(area.x, y, area.width, 1),
                        Style::new().bg(bg),
                    );
                    let shown = sidebar::cut(&format!(" {text}"), usize::from(area.width));
                    buffer.set_stringn(
                        area.x,
                        y,
                        &shown,
                        usize::from(area.width),
                        Style::new().fg(moonlight::CYAN).bg(bg),
                    );
                    continue;
                }
                UnifiedRow::BinaryNotice { .. } => {
                    buffer.set_stringn(
                        area.x,
                        y,
                        " Binary file changed",
                        usize::from(area.width),
                        Style::new().fg(moonlight::YELLOW),
                    );
                    continue;
                }
                UnifiedRow::Context {
                    old_line,
                    new_line,
                    at,
                } => {
                    let line = at.get(viewer.patch());
                    (
                        format_line_number(*old_line),
                        format_line_number(*new_line),
                        " ",
                        moonlight::MUTE,
                        row_bg(Color::Reset, is_cursor, is_selected),
                        None,
                        line.text.as_str(),
                        line.syntax.as_slice(),
                        &[][..],
                    )
                }
                UnifiedRow::Deleted { old_line, at } => {
                    let line = at.get(viewer.patch());
                    (
                        format_line_number(*old_line),
                        blank_line_number(),
                        "-",
                        moonlight::RED,
                        row_bg(DEL_BG, is_cursor, is_selected),
                        Some(DEL_EMPH_BG),
                        line.text.as_str(),
                        line.syntax.as_slice(),
                        line.emphasis.as_slice(),
                    )
                }
                UnifiedRow::Added { new_line, at } => {
                    let line = at.get(viewer.patch());
                    (
                        blank_line_number(),
                        format_line_number(*new_line),
                        "+",
                        moonlight::GREEN,
                        row_bg(ADD_BG, is_cursor, is_selected),
                        Some(ADD_EMPH_BG),
                        line.text.as_str(),
                        line.syntax.as_slice(),
                        line.emphasis.as_slice(),
                    )
                }
            };

        if base_bg != Color::Reset {
            fill_row(
                buffer,
                Rect::new(area.x, y, area.width, 1),
                Style::new().bg(base_bg),
            );
        }
        let gutter = Line::from(vec![
            Span::styled(old_text, Style::new().fg(moonlight::MUTE).bg(base_bg)),
            Span::styled(" ", Style::new().bg(base_bg)),
            Span::styled(new_text, Style::new().fg(moonlight::MUTE).bg(base_bg)),
            Span::styled(" ", Style::new().bg(base_bg)),
            Span::styled(
                marker,
                Style::new()
                    .fg(marker_fg)
                    .bg(base_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::new().bg(base_bg)),
        ]);
        buffer.set_line(area.x, y, &gutter, area.width.min(prefix_width));
        if area.width <= prefix_width {
            continue;
        }
        let code_rect = Rect::new(area.x + prefix_width, y, area.width - prefix_width, 1);
        draw_styled_code(
            buffer,
            code_rect,
            text,
            syntax,
            emphasis,
            base_bg,
            emphasis_bg,
            search_query,
        );
    }
}

fn draw_file_banner(
    buffer: &mut Buffer,
    area: Rect,
    viewer: &Viewer,
    file_index: usize,
    is_cursor: bool,
) {
    assert!(
        file_index < viewer.patch().files.len(),
        "banner file index in bounds"
    );
    let file = &viewer.patch().files[file_index];
    let bg = if is_cursor {
        moonlight::LIFTED
    } else {
        moonlight::SURFACE
    };
    fill_row(buffer, area, Style::new().bg(bg));
    let seen = viewer.is_viewed(file_index);
    let badge = if seen { "✓" } else { file.status().badge() };
    let badge_color = if seen {
        moonlight::GREEN
    } else {
        moonlight::YELLOW
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {badge} "),
            Style::new()
                .fg(badge_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            file.path().as_str().to_owned(),
            Style::new()
                .fg(moonlight::TEXT)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  +{}", file.added),
            Style::new().fg(moonlight::GREEN).bg(bg),
        ),
        Span::styled(
            format!(" -{}", file.deleted),
            Style::new().fg(moonlight::RED).bg(bg),
        ),
    ]);
    buffer.set_line(area.x, area.y, &line, area.width);
}

fn draw_styled_code(
    buffer: &mut Buffer,
    rect: Rect,
    text: &str,
    syntax: &[TokenSpan],
    emphasis: &[ByteSpan],
    base_bg: Color,
    emphasis_bg: Option<Color>,
    search_query: Option<&str>,
) {
    if rect.is_empty() || text.is_empty() {
        return;
    }
    let search_range = search_query.and_then(|needle| find_case_insensitive_range(text, needle));
    let max_cols = usize::from(rect.width);
    let mut column = 0usize;
    let mut syntax_index = 0usize;
    let mut emphasis_index = 0usize;

    for (byte_index, ch) in text.char_indices() {
        let (display, char_width) = if ch == '\t' {
            ("    ", 4)
        } else if ch.is_control() {
            continue;
        } else {
            ("", ch.width().unwrap_or(0))
        };
        if char_width == 0 {
            continue;
        }
        if column + char_width > max_cols {
            if max_cols >= 1 && column < max_cols {
                let tail = u16::try_from(max_cols - 1).expect("rect width fits u16");
                buffer.set_string(
                    rect.x + tail,
                    rect.y,
                    "…",
                    Style::new().fg(moonlight::MUTE).bg(base_bg),
                );
            }
            break;
        }
        while syntax_index < syntax.len() && syntax[syntax_index].end <= byte_index {
            syntax_index += 1;
        }
        while emphasis_index < emphasis.len() && emphasis[emphasis_index].end <= byte_index {
            emphasis_index += 1;
        }
        let role = syntax
            .get(syntax_index)
            .filter(|span| byte_index >= span.start && byte_index < span.end)
            .map_or(SyntaxRole::Plain, |span| span.role);
        let in_emphasis = emphasis_bg.is_some()
            && emphasis
                .get(emphasis_index)
                .is_some_and(|span| byte_index >= span.start && byte_index < span.end);
        let in_search = search_range
            .as_ref()
            .is_some_and(|range| range.contains(&byte_index));
        let bg = if in_search {
            SEARCH_BG
        } else if in_emphasis {
            emphasis_bg.unwrap_or(base_bg)
        } else {
            base_bg
        };
        let fg = role_color(role);
        let mut style = Style::new().fg(fg).bg(bg);
        if in_search {
            style = style.fg(moonlight::YELLOW).add_modifier(Modifier::BOLD);
        }
        let x_offset = u16::try_from(column).expect("code column fits u16");
        let x = rect.x + x_offset;
        if display.is_empty() {
            let mut utf8 = [0u8; 4];
            buffer.set_string(x, rect.y, ch.encode_utf8(&mut utf8), style);
        } else {
            buffer.set_string(x, rect.y, display, style);
        }
        column += char_width;
    }
}

fn role_color(role: SyntaxRole) -> Color {
    match role {
        SyntaxRole::Plain | SyntaxRole::Punctuation => moonlight::TEXT,
        SyntaxRole::Keyword => moonlight::MAUVE,
        SyntaxRole::Type => moonlight::BLUE,
        SyntaxRole::String => moonlight::GREEN,
        SyntaxRole::Number => moonlight::PEACH,
        SyntaxRole::Comment => moonlight::MUTE,
        SyntaxRole::Macro | SyntaxRole::Heading | SyntaxRole::Operator => moonlight::CYAN,
        SyntaxRole::Attribute => moonlight::YELLOW,
    }
}

fn row_bg(base: Color, is_cursor: bool, is_selected: bool) -> Color {
    if is_selected {
        SELECT_BG
    } else if is_cursor {
        if base == Color::Reset {
            CURSOR_BG
        } else {
            base
        }
    } else {
        base
    }
}

fn fill_row(buffer: &mut Buffer, area: Rect, style: Style) {
    for x in area.left()..area.right() {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol(" ");
            cell.set_style(style);
        }
    }
}

pub fn emit_kitty_ribbon(out: &mut impl io::Write, overlay: &RibbonOverlay) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(overlay.image.rgba());
    let bytes = encoded.as_bytes();
    let row = overlay.rect.y.saturating_add(1);
    let col = overlay.rect.x.saturating_add(1);
    write!(out, "\x1b7\x1b[{row};{col}H")?;
    let mut offset = 0usize;
    let mut first = true;
    while offset < bytes.len() {
        let end = (offset + KITTY_CHUNK_BYTES).min(bytes.len());
        let more = u8::from(end < bytes.len());
        let chunk = std::str::from_utf8(&bytes[offset..end]).expect("base64 is ascii");
        if first {
            write!(
                out,
                "\x1b_Ga=T,f=32,s={},v={},c={},r={},i={},p=1,q=2,C=1,m={more};{chunk}\x1b\\",
                overlay.image.width(),
                overlay.image.height(),
                overlay.rect.width,
                overlay.rect.height,
                overlay.image_id,
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};{chunk}\x1b\\")?;
        }
        offset = end;
    }
    assert_eq!(offset, bytes.len(), "entire base64 payload emitted");
    write!(out, "\x1b8")
}

pub fn clear_kitty_ribbon(out: &mut impl io::Write, image_id: NonZeroU32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";

    fn rev(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).unwrap()
    }

    fn kitty() -> KittyTarget {
        KittyTarget {
            cell_px: (NonZeroU16::new(8).unwrap(), NonZeroU16::new(16).unwrap()),
            image_id: KITTY_BASE_ID,
        }
    }

    fn loaded_diff(patch: &str) -> Arc<LoadedDiff> {
        Arc::new(
            LoadedDiff::of_wire(ReadDiffResponse {
                base: "main".to_owned(),
                head: "feat".to_owned(),
                patch: patch.as_bytes().to_vec(),
                truncated: false,
                untracked: Vec::new(),
            })
            .unwrap(),
        )
    }

    #[test]
    fn draws_split_view_with_unicode_fallback_and_kitty_overlay() {
        let mut pane = ReviewPane::loading();
        assert!(pane.loaded(Ok(loaded_diff(SAMPLE)), rev(1)));
        assert_eq!(pane.tab_label(), "review +1 -1");

        let area = Rect::new(0, 0, 110, 10);
        let mut buf = Buffer::empty(area);
        let none = pane.draw(&mut buf, area, true, None);
        assert!(none.is_none());
        let header: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(header.contains("main → feat"), "{header}");
        assert!(header.contains("+1 -1"), "{header}");

        let mut buf_kitty = Buffer::empty(area);
        let overlay = pane.draw(&mut buf_kitty, area, true, Some(kitty()));
        let overlay = overlay.expect("split review emits kitty ribbon overlay");
        assert_eq!(overlay.rect.width.get(), GUTTER_COLS);
        let mut wire = Vec::new();
        emit_kitty_ribbon(&mut wire, &overlay).unwrap();
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.contains("\x1b_Ga=T,f=32,"));
        assert!(wire.contains(",q=2,C=1,"));

        let second = pane
            .draw(&mut buf_kitty, area, true, Some(kitty()))
            .expect("cached overlay reuses rasterized image");
        assert!(Arc::ptr_eq(&overlay.image, &second.image));
    }

    #[test]
    fn stale_revision_does_not_overwrite_newer_loaded_or_failed_diff() {
        let mut pane = ReviewPane::loading();
        let newer = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,3 @@
 a
+b
+c
";
        assert!(pane.loaded(Ok(loaded_diff(newer)), rev(5)));
        assert_eq!(pane.tab_label(), "review +2 -0");
        assert!(!pane.loaded(Ok(loaded_diff(SAMPLE)), rev(4)));
        assert_eq!(pane.tab_label(), "review +2 -0");

        let mut failed_pane = ReviewPane::loading();
        assert!(failed_pane.loaded(Err("newer failure".to_owned()), rev(5)));
        assert!(!failed_pane.loaded(Ok(loaded_diff(SAMPLE)), rev(4)));
        assert!(matches!(
            failed_pane.state,
            PaneState::Failed { revision, .. } if revision == rev(5)
        ));
    }

    #[test]
    fn keys_drive_explain_ask_and_narrow_pane_never_disagrees_on_layout() {
        let mut pane = ReviewPane::loading();
        assert!(pane.loaded(Ok(loaded_diff(SAMPLE)), rev(1)));
        let area = Rect::new(0, 0, 110, 10);
        let mut buf = Buffer::empty(area);
        pane.draw(&mut buf, area, true, None);

        pane.key(&KeyEvent::from(KeyCode::Char('j')));
        pane.key(&KeyEvent::from(KeyCode::Char('j')));
        pane.key(&KeyEvent::from(KeyCode::Char('j')));
        let explain = pane.key(&KeyEvent::from(KeyCode::Char('e')));
        assert!(matches!(explain, ReviewAction::Explain(ref ex) if ex.path() == "src/lib.rs"));
        let ask = pane.key(&KeyEvent::from(KeyCode::Char('?')));
        assert!(matches!(ask, ReviewAction::Ask(ref ex) if ex.path() == "src/lib.rs"));

        let narrow = Rect::new(0, 0, SPLIT_HARD_MIN_WIDTH - 1, 10);
        let mut narrow_buf = Buffer::empty(narrow);
        pane.draw(&mut narrow_buf, narrow, true, None);
        let header: String = (0..narrow.width)
            .map(|x| narrow_buf[(x, 0)].symbol())
            .collect();
        assert!(!header.contains("[split]"), "{header}");

        let mut clear_wire = Vec::new();
        clear_kitty_ribbon(&mut clear_wire, KITTY_BASE_ID).unwrap();
        assert_eq!(
            String::from_utf8(clear_wire).unwrap(),
            format!("\x1b_Ga=d,d=I,i={KITTY_BASE_ID},q=2\x1b\\")
        );
    }

    #[test]
    fn folded_directory_shows_collapsed_chevron_and_five_digit_lines_keep_unified_markers() {
        let patch = "\
diff --git a/crates/a/one.rs b/crates/a/one.rs
--- a/crates/a/one.rs
+++ a/crates/a/one.rs
@@ -10000,2 +10000,2 @@
-old_big();
+new_big();
 ctx();
diff --git a/crates/a/two.rs b/crates/a/two.rs
--- a/crates/a/two.rs
+++ a/crates/a/two.rs
@@ -1 +1,2 @@
 keep();
+more();
";
        let mut pane = ReviewPane::loading();
        assert!(pane.loaded(Ok(loaded_diff(patch)), rev(1)));
        let area = Rect::new(0, 0, 120, 10);
        let mut buf = Buffer::empty(area);
        pane.draw(&mut buf, area, true, None);
        let row1: String = (0..28).map(|x| buf[(x, 1)].symbol()).collect();
        assert!(row1.contains('▾'), "{row1}");
        assert!(row1.contains("+2 -1"), "{row1}");
        let split_row: String = (29..120).map(|x| buf[(x, 3)].symbol()).collect();
        assert!(split_row.contains("10000"), "{split_row}");

        pane.key(&KeyEvent::from(KeyCode::Tab));
        assert_eq!(
            pane.key(&KeyEvent::from(KeyCode::Char('e'))),
            ReviewAction::None
        );
        let mut tree_buf = Buffer::empty(area);
        pane.draw(&mut tree_buf, area, true, None);
        let tree_hints: String = (0..area.width)
            .map(|x| tree_buf[(x, area.height - 1)].symbol())
            .collect();
        assert!(tree_hints.contains("FILES"), "{tree_hints}");
        assert!(!tree_hints.contains("e explain"), "{tree_hints}");
        assert!(!tree_hints.contains("? ask"), "{tree_hints}");
        pane.key(&KeyEvent::from(KeyCode::Enter));
        let mut folded_buf = Buffer::empty(area);
        pane.draw(&mut folded_buf, area, true, None);
        let folded_row1: String = (0..28).map(|x| folded_buf[(x, 1)].symbol()).collect();
        assert!(folded_row1.contains('▸'), "{folded_row1}");

        pane.key(&KeyEvent::from(KeyCode::Tab));
        pane.key(&KeyEvent::from(KeyCode::Char('s')));
        let mut unified_buf = Buffer::empty(area);
        pane.draw(&mut unified_buf, area, true, None);
        let del_row: String = (29..80).map(|x| unified_buf[(x, 3)].symbol()).collect();
        assert!(del_row.contains("10000"), "{del_row}");
        assert!(del_row.contains(" - old_big();"), "{del_row}");
        assert_eq!(format_line_number(123_456), "…3456");

        let mut loading_pane = ReviewPane::loading();
        let mut loading_buf = Buffer::empty(area);
        loading_pane.draw(&mut loading_buf, area, true, None);
        let loading_hints: String = (0..area.width)
            .map(|x| loading_buf[(x, area.height - 1)].symbol())
            .collect();
        assert!(loading_hints.contains("r reload"), "{loading_hints}");
        assert!(!loading_hints.contains("e explain"), "{loading_hints}");

        pane.key(&KeyEvent::from(KeyCode::Char('/')));
        pane.key(&KeyEvent::from(KeyCode::Char('k')));
        pane.key(&KeyEvent::from(KeyCode::Enter));
        pane.key(&KeyEvent::from(KeyCode::Char('v')));
        pane.key(&KeyEvent::from(KeyCode::Tab));
        let mut tree_over_search_buf = Buffer::empty(area);
        pane.draw(&mut tree_over_search_buf, area, true, None);
        let tree_over_search: String = (0..area.width)
            .map(|x| tree_over_search_buf[(x, area.height - 1)].symbol())
            .collect();
        assert!(tree_over_search.contains("FILES"), "{tree_over_search}");
        pane.key(&KeyEvent::from(KeyCode::Char('v')));
        pane.key(&KeyEvent::from(KeyCode::Char('/')));
        pane.key(&KeyEvent::from(KeyCode::Esc));
        let mut after_esc_buf = Buffer::empty(area);
        pane.draw(&mut after_esc_buf, area, true, None);
        let after_esc: String = (0..area.width)
            .map(|x| after_esc_buf[(x, area.height - 1)].symbol())
            .collect();
        assert!(
            !after_esc.contains("FILES"),
            "Esc leaves FILES even after v and / in tree focus: {after_esc}"
        );
    }
}
