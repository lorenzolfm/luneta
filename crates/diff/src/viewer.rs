use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use crate::layout::{LeftRow, RightRow, SplitLayout, SplitLine, UnifiedLayout, UnifiedRow};
use crate::patch::{Block, FileBody, FileChange, FilePatch, FilePath, Hunk, LINE_BYTES_MAX, Patch};
use crate::ribbon::GUTTER_COLS;
use crate::tree::{FileTree, TreeNode};

pub const LINE_NUMBER_COLS: u16 = 5;

pub const SPLIT_MIN_WIDTH: u16 = 88;

pub const SPLIT_HARD_MIN_WIDTH: u16 = LINE_NUMBER_COLS * 2 + GUTTER_COLS + 7;

pub const SEARCH_QUERY_BYTES_MAX: usize = 256;

const SEARCH_MATCHES_MAX: usize = 4096;

pub const EXCERPT_LINES_MAX: usize = 200;

pub const EXCERPT_BYTES_MAX: usize = 16 * 1024;

const EXCERPT_TRUNCATED_MARKER: &str = "... [truncated]";

const EXCERPT_NEWLINE_BYTES: usize = 1;

const EXCERPT_CONTENT_BYTES_MAX: usize =
    EXCERPT_BYTES_MAX - EXCERPT_TRUNCATED_MARKER.len() - EXCERPT_NEWLINE_BYTES;

const _: () = assert!(SPLIT_HARD_MIN_WIDTH <= SPLIT_MIN_WIDTH);
const _: () = assert!(SEARCH_QUERY_BYTES_MAX > 0);
const _: () = assert!(SEARCH_MATCHES_MAX > 0);
const _: () = assert!(EXCERPT_LINES_MAX > 0);
const _: () = assert!(EXCERPT_BYTES_MAX >= EXCERPT_TRUNCATED_MARKER.len() + 64);
const _: () = assert!(
    EXCERPT_CONTENT_BYTES_MAX + EXCERPT_NEWLINE_BYTES + EXCERPT_TRUNCATED_MARKER.len()
        == EXCERPT_BYTES_MAX
);
const _: () =
    assert!(LINE_BYTES_MAX + "Renamed from  with no content changes".len() <= EXCERPT_BYTES_MAX);
const _: () = assert!(LINE_BYTES_MAX + 64 <= EXCERPT_CONTENT_BYTES_MAX);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutMode {
    #[default]
    Auto,
    Split,
    Unified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLayout {
    Split,
    Unified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sidebar {
    Closed,
    Open { focused: bool },
}

impl Sidebar {
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Sidebar::Open { .. })
    }

    #[must_use]
    pub const fn is_focused(self) -> bool {
        matches!(self, Sidebar::Open { focused: true })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatches {
    matches: Vec<SearchMatch>,
    current: usize,
}

impl SearchMatches {
    fn new(matches: Vec<SearchMatch>, cursor: usize) -> Option<SearchMatches> {
        if matches.is_empty() {
            return None;
        }
        let current = matches
            .iter()
            .position(|entry| entry.row >= cursor)
            .unwrap_or(0);
        Some(SearchMatches { matches, current })
    }

    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    #[must_use]
    pub fn count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.matches.len()).expect("SearchMatches is non-empty")
    }

    #[must_use]
    pub const fn current_index(&self) -> usize {
        self.current
    }

    #[must_use]
    pub fn current(&self) -> SearchMatch {
        self.matches[self.current]
    }

    fn step_next(&mut self) -> SearchMatch {
        self.current = (self.current + 1) % self.matches.len();
        self.matches[self.current]
    }

    fn step_prev(&mut self) -> SearchMatch {
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
        self.matches[self.current]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SearchState {
    #[default]
    Idle,
    Editing {
        query: String,
        found: Option<SearchMatches>,
    },
    Committed {
        query: String,
        found: Option<SearchMatches>,
    },
}

impl SearchState {
    #[must_use]
    pub const fn is_editing(&self) -> bool {
        matches!(self, SearchState::Editing { .. })
    }

    #[must_use]
    pub const fn is_visible(&self) -> bool {
        !matches!(self, SearchState::Idle)
    }

    #[must_use]
    pub fn query(&self) -> Option<&str> {
        match self {
            SearchState::Idle => None,
            SearchState::Editing { query, .. } => (!query.is_empty()).then_some(query.as_str()),
            SearchState::Committed { query, .. } => Some(query.as_str()),
        }
    }

    #[must_use]
    pub fn found(&self) -> Option<&SearchMatches> {
        match self {
            SearchState::Idle => None,
            SearchState::Editing { found, .. } | SearchState::Committed { found, .. } => {
                found.as_ref()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    start: u32,
    end: u32,
}

impl LineRange {
    #[must_use]
    pub const fn single(line: u32) -> LineRange {
        LineRange {
            start: line,
            end: line,
        }
    }

    #[must_use]
    pub fn new(start: u32, end: u32) -> Option<LineRange> {
        (start <= end).then_some(LineRange { start, end })
    }

    pub fn extend(&mut self, line: u32) {
        self.start = self.start.min(line);
        self.end = self.end.max(line);
    }

    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionExcerpt {
    path: FilePath,
    lines: Option<LineRange>,
    snippet: String,
    enclosing_hunks: Option<String>,
}

impl SelectionExcerpt {
    #[must_use]
    pub fn new(
        path: FilePath,
        lines: Option<LineRange>,
        snippet: String,
        enclosing_hunks: Option<String>,
    ) -> Option<SelectionExcerpt> {
        let snippet = strip_nul(snippet);
        if snippet.is_empty() || snippet.len() > EXCERPT_BYTES_MAX {
            return None;
        }
        let enclosing_hunks = match enclosing_hunks {
            Some(text) => {
                let cleaned = strip_nul(text);
                if cleaned.len() > EXCERPT_BYTES_MAX {
                    return None;
                }
                (!cleaned.is_empty() && cleaned != snippet).then_some(cleaned)
            }
            None => None,
        };
        assert!(!snippet.is_empty(), "selection excerpt is non-empty");
        assert!(
            snippet.len() <= EXCERPT_BYTES_MAX,
            "selection excerpt stays within EXCERPT_BYTES_MAX"
        );
        assert!(
            !snippet.as_bytes().contains(&0),
            "selection excerpt contains no NUL bytes"
        );
        if let Some(enclosing) = &enclosing_hunks {
            assert!(
                !enclosing.is_empty(),
                "enclosing hunks excerpt is non-empty"
            );
            assert!(
                enclosing.len() <= EXCERPT_BYTES_MAX,
                "enclosing hunks excerpt stays within EXCERPT_BYTES_MAX"
            );
            assert!(
                !enclosing.as_bytes().contains(&0),
                "enclosing hunks excerpt contains no NUL bytes"
            );
            assert_ne!(
                enclosing.as_str(),
                snippet.as_str(),
                "enclosing hunks excerpt differs from snippet"
            );
        }
        Some(SelectionExcerpt {
            path,
            lines,
            snippet,
            enclosing_hunks,
        })
    }

    #[must_use]
    pub fn path(&self) -> &FilePath {
        &self.path
    }

    #[must_use]
    pub const fn lines(&self) -> Option<LineRange> {
        self.lines
    }

    #[must_use]
    pub fn snippet(&self) -> &str {
        assert!(!self.snippet.is_empty(), "selection excerpt is non-empty");
        assert!(
            self.snippet.len() <= EXCERPT_BYTES_MAX,
            "selection excerpt stays within EXCERPT_BYTES_MAX"
        );
        &self.snippet
    }

    #[must_use]
    pub fn enclosing_hunks(&self) -> Option<&str> {
        if let Some(enclosing) = &self.enclosing_hunks {
            assert!(
                !enclosing.is_empty(),
                "enclosing hunks excerpt is non-empty"
            );
            assert!(
                enclosing.len() <= EXCERPT_BYTES_MAX,
                "enclosing hunks excerpt stays within EXCERPT_BYTES_MAX"
            );
            assert_ne!(
                enclosing.as_str(),
                self.snippet.as_str(),
                "enclosing hunks excerpt differs from snippet"
            );
        }
        self.enclosing_hunks.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct Viewer {
    patch: Arc<Patch>,
    split: SplitLayout,
    unified: UnifiedLayout,
    tree: FileTree,
    base: String,
    head: String,
    mode: LayoutMode,
    active_layout: ActiveLayout,
    sidebar: Sidebar,
    scroll: usize,
    cursor: usize,
    tree_cursor: usize,
    selection_anchor: Option<usize>,
    search: SearchState,
    viewed: BTreeSet<String>,
    folded: BTreeSet<String>,
}

fn effective_layout(mode: LayoutMode, diff_width: u16) -> ActiveLayout {
    if diff_width < SPLIT_HARD_MIN_WIDTH {
        return ActiveLayout::Unified;
    }
    match mode {
        LayoutMode::Auto => {
            if diff_width >= SPLIT_MIN_WIDTH {
                ActiveLayout::Split
            } else {
                ActiveLayout::Unified
            }
        }
        LayoutMode::Split => ActiveLayout::Split,
        LayoutMode::Unified => ActiveLayout::Unified,
    }
}

impl Viewer {
    #[must_use]
    pub fn new(patch: impl Into<Arc<Patch>>, base: String, head: String) -> Viewer {
        let patch = patch.into();
        let split = SplitLayout::build(&patch);
        let unified = UnifiedLayout::build(&patch);
        assert_eq!(
            split.files.len(),
            patch.files.len(),
            "split layout covers all files"
        );
        assert_eq!(
            unified.files.len(),
            patch.files.len(),
            "unified layout covers all files"
        );
        let folded = BTreeSet::new();
        let tree = FileTree::build(&patch, &folded);
        let sidebar = if patch.files.len() > 1 {
            Sidebar::Open { focused: false }
        } else {
            Sidebar::Closed
        };
        Viewer {
            patch,
            split,
            unified,
            tree,
            base,
            head,
            mode: LayoutMode::Auto,
            active_layout: ActiveLayout::Split,
            sidebar,
            scroll: 0,
            cursor: 0,
            tree_cursor: 0,
            selection_anchor: None,
            search: SearchState::Idle,
            viewed: BTreeSet::new(),
            folded,
        }
    }

    #[must_use]
    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    #[must_use]
    pub fn split(&self) -> &SplitLayout {
        &self.split
    }

    #[must_use]
    pub fn unified(&self) -> &UnifiedLayout {
        &self.unified
    }

    #[must_use]
    pub fn tree(&self) -> &FileTree {
        &self.tree
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    #[must_use]
    pub const fn mode(&self) -> LayoutMode {
        self.mode
    }

    #[must_use]
    pub const fn active_layout(&self) -> ActiveLayout {
        self.active_layout
    }

    #[must_use]
    pub const fn is_split(&self) -> bool {
        matches!(self.active_layout, ActiveLayout::Split)
    }

    #[must_use]
    pub const fn sidebar(&self) -> Sidebar {
        self.sidebar
    }

    #[must_use]
    pub const fn scroll(&self) -> usize {
        self.scroll
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub const fn tree_cursor(&self) -> usize {
        self.tree_cursor
    }

    #[must_use]
    pub const fn is_selecting(&self) -> bool {
        self.selection_anchor.is_some()
    }

    #[must_use]
    pub fn search(&self) -> &SearchState {
        &self.search
    }

    #[must_use]
    pub fn viewed_count(&self) -> usize {
        self.viewed.len()
    }

    pub fn reload(&mut self, patch: impl Into<Arc<Patch>>, base: String, head: String) {
        let anchor = self.cursor_location();
        self.patch = patch.into();
        self.base = base;
        self.head = head;
        self.split = SplitLayout::build(&self.patch);
        self.unified = UnifiedLayout::build(&self.patch);
        assert_eq!(
            self.split.files.len(),
            self.patch.files.len(),
            "reloaded split covers files"
        );
        assert_eq!(
            self.unified.files.len(),
            self.patch.files.len(),
            "reloaded unified covers files"
        );
        self.tree = FileTree::build(&self.patch, &self.folded);
        if self.tree.entries.is_empty() {
            self.unfocus_sidebar();
        }
        let present: BTreeSet<&str> = self
            .patch
            .files
            .iter()
            .map(|file| file.path().as_str())
            .collect();
        self.viewed.retain(|path| present.contains(path.as_str()));
        self.selection_anchor = None;
        if let Some((path, line)) = anchor {
            self.restore_cursor(&path, line);
        } else {
            self.clamp_cursor();
        }
        self.recompute_search();
        if self.tree_cursor >= self.tree.entries.len() {
            self.tree_cursor = self.tree.entries.len().saturating_sub(1);
        }
    }

    pub fn sync_tree_availability(&mut self, can_show_tree: bool) {
        if !can_show_tree || self.tree.entries.is_empty() {
            self.unfocus_sidebar();
        }
    }

    pub fn sync_layout(&mut self, diff_width: u16) {
        let next = effective_layout(self.mode, diff_width);
        if next != self.active_layout {
            self.switch_layout(next);
        }
    }

    fn switch_layout(&mut self, next: ActiveLayout) {
        let anchor = self.cursor_location();
        self.active_layout = next;
        self.selection_anchor = None;
        if let Some((path, line)) = anchor {
            self.restore_cursor(&path, line);
        } else {
            self.clamp_cursor();
        }
        self.recompute_search();
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        match self.active_layout {
            ActiveLayout::Split => self.split.row_count(),
            ActiveLayout::Unified => self.unified.rows.len(),
        }
    }

    pub fn toggle_layout(&mut self, diff_width: u16) {
        self.sync_layout(diff_width);
        if diff_width < SPLIT_HARD_MIN_WIDTH {
            return;
        }
        self.mode = match self.active_layout {
            ActiveLayout::Split => LayoutMode::Unified,
            ActiveLayout::Unified => LayoutMode::Split,
        };
        self.sync_layout(diff_width);
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar = match self.sidebar {
            Sidebar::Closed => Sidebar::Open { focused: false },
            Sidebar::Open { .. } => Sidebar::Closed,
        };
    }

    pub fn toggle_focus(&mut self, can_show_tree: bool) {
        if !can_show_tree || self.tree.entries.is_empty() {
            self.unfocus_sidebar();
            return;
        }
        self.sidebar = match self.sidebar {
            Sidebar::Closed | Sidebar::Open { focused: false } => Sidebar::Open { focused: true },
            Sidebar::Open { focused: true } => Sidebar::Open { focused: false },
        };
    }

    pub fn unfocus_sidebar(&mut self) {
        if let Sidebar::Open { focused } = &mut self.sidebar {
            *focused = false;
        }
    }

    pub fn toggle_viewed(&mut self) {
        let Some(file_index) = self.current_file() else {
            return;
        };
        assert!(
            file_index < self.patch.files.len(),
            "current file index in bounds"
        );
        let path = self.patch.files[file_index].path().as_str().to_owned();
        if !self.viewed.remove(&path) {
            self.viewed.insert(path);
        }
    }

    #[must_use]
    pub fn is_viewed(&self, file_index: usize) -> bool {
        self.patch
            .files
            .get(file_index)
            .is_some_and(|file| self.viewed.contains(file.path().as_str()))
    }

    #[must_use]
    pub fn is_folded(&self, path: &str) -> bool {
        self.folded.contains(path)
    }

    #[must_use]
    fn diff_file_at(&self, row: usize) -> Option<usize> {
        match self.active_layout {
            ActiveLayout::Split => self
                .split
                .rows
                .get(row)
                .and_then(SplitLine::file_index)
                .or_else(|| {
                    self.split
                        .files
                        .iter()
                        .find(|span| row >= span.start_row && row < span.end_row)
                        .map(|span| span.file)
                }),
            ActiveLayout::Unified => self.unified.rows.get(row).map(UnifiedRow::file_index),
        }
    }

    #[must_use]
    pub fn current_file(&self) -> Option<usize> {
        if self.sidebar.is_focused()
            && let Some(entry) = self.tree.entries.get(self.tree_cursor)
            && let TreeNode::File { file, .. } = entry.node
        {
            return Some(file);
        }
        self.diff_file_at(self.cursor)
    }

    pub fn move_down(&mut self, step: usize, viewport: usize) {
        if self.sidebar.is_focused() {
            let max = self.tree.entries.len().saturating_sub(1);
            self.tree_cursor = self.tree_cursor.saturating_add(step).min(max);
            return;
        }
        let max = self.row_count().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(step).min(max);
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn move_up(&mut self, step: usize, viewport: usize) {
        if self.sidebar.is_focused() {
            self.tree_cursor = self.tree_cursor.saturating_sub(step);
            return;
        }
        self.cursor = self.cursor.saturating_sub(step);
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn scroll_tree_by(&mut self, delta: isize) {
        let max = self.tree.entries.len().saturating_sub(1);
        self.tree_cursor = if delta >= 0 {
            self.tree_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max)
        } else {
            self.tree_cursor.saturating_sub(delta.unsigned_abs())
        };
    }

    pub fn scroll_by(&mut self, delta: isize, viewport: usize) {
        let count = self.row_count();
        let max_scroll = count.saturating_sub(viewport.max(1));
        self.scroll = if delta >= 0 {
            self.scroll
                .saturating_add(delta.unsigned_abs())
                .min(max_scroll)
        } else {
            self.scroll.saturating_sub(delta.unsigned_abs())
        };
        if count > 0 {
            let end = self
                .scroll
                .saturating_add(viewport.saturating_sub(1))
                .min(count - 1);
            self.cursor = self.cursor.clamp(self.scroll, end);
            assert!(
                self.cursor < count,
                "cursor remains within rows after scroll"
            );
            self.sync_tree_cursor();
        }
    }

    pub fn top(&mut self, viewport: usize) {
        if self.sidebar.is_focused() {
            self.tree_cursor = 0;
            return;
        }
        self.cursor = 0;
        self.scroll = 0;
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn bottom(&mut self, viewport: usize) {
        if self.sidebar.is_focused() {
            self.tree_cursor = self.tree.entries.len().saturating_sub(1);
            return;
        }
        self.cursor = self.row_count().saturating_sub(1);
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn next_hunk(&mut self, viewport: usize) {
        let hunks = match self.active_layout {
            ActiveLayout::Split => &self.split.hunks,
            ActiveLayout::Unified => &self.unified.hunks,
        };
        if let Some(target) = hunks.iter().find(|hunk| hunk.row > self.cursor) {
            self.cursor = target.row;
            self.scroll = target.row.saturating_sub(2);
            self.keep_visible(viewport);
            self.sync_tree_cursor();
        }
    }

    pub fn prev_hunk(&mut self, viewport: usize) {
        let hunks = match self.active_layout {
            ActiveLayout::Split => &self.split.hunks,
            ActiveLayout::Unified => &self.unified.hunks,
        };
        if let Some(target) = hunks.iter().rev().find(|hunk| hunk.row < self.cursor) {
            self.cursor = target.row;
            self.scroll = target.row.saturating_sub(2);
            self.keep_visible(viewport);
            self.sync_tree_cursor();
        }
    }

    pub fn next_file(&mut self, viewport: usize) {
        let files = match self.active_layout {
            ActiveLayout::Split => &self.split.files,
            ActiveLayout::Unified => &self.unified.files,
        };
        if let Some(target) = files.iter().find(|file| file.start_row > self.cursor) {
            self.jump_row(target.start_row, viewport);
        }
    }

    pub fn prev_file(&mut self, viewport: usize) {
        let files = match self.active_layout {
            ActiveLayout::Split => &self.split.files,
            ActiveLayout::Unified => &self.unified.files,
        };
        if let Some(target) = files.iter().rev().find(|file| file.start_row < self.cursor) {
            self.jump_row(target.start_row, viewport);
        }
    }

    pub fn activate_tree_index(&mut self, index: usize, viewport: usize) {
        assert!(viewport > 0, "review viewport has at least one row");
        if index < self.tree.entries.len() {
            self.tree_cursor = index;
            self.activate_tree_entry(viewport);
            if !self.tree.entries.is_empty() {
                assert!(
                    self.tree_cursor < self.tree.entries.len(),
                    "tree cursor stays within tree entries"
                );
            }
        }
    }

    pub fn activate_tree_entry(&mut self, viewport: usize) {
        let Some(entry) = self.tree.entries.get(self.tree_cursor).cloned() else {
            return;
        };
        match entry.node {
            TreeNode::Dir { path, .. } => {
                if !self.folded.remove(&path) {
                    self.folded.insert(path);
                }
                self.tree = FileTree::build(&self.patch, &self.folded);
                if self.tree_cursor >= self.tree.entries.len() {
                    self.tree_cursor = self.tree.entries.len().saturating_sub(1);
                }
            }
            TreeNode::File { file, .. } => {
                self.jump_to_file(file, viewport);
                self.unfocus_sidebar();
            }
        }
    }

    pub fn click_diff_row(&mut self, row: usize, viewport: usize) {
        if row >= self.row_count() {
            return;
        }
        self.unfocus_sidebar();
        self.cursor = row;
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn jump_to_file(&mut self, file: usize, viewport: usize) {
        let files = match self.active_layout {
            ActiveLayout::Split => &self.split.files,
            ActiveLayout::Unified => &self.unified.files,
        };
        if let Some(span) = files.iter().find(|span| span.file == file) {
            self.jump_row(span.start_row, viewport);
        }
    }

    fn jump_row(&mut self, row: usize, viewport: usize) {
        self.cursor = row;
        self.scroll = row;
        self.selection_anchor = None;
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn toggle_select(&mut self) {
        self.selection_anchor = match self.selection_anchor {
            Some(_) => None,
            None => Some(self.cursor),
        };
    }

    pub fn clear_select(&mut self) {
        self.selection_anchor = None;
    }

    #[must_use]
    pub fn selection_range(&self) -> Option<RangeInclusive<usize>> {
        let anchor = self.selection_anchor?;
        let range = if anchor <= self.cursor {
            anchor..=self.cursor
        } else {
            self.cursor..=anchor
        };
        assert!(range.start() <= range.end(), "selection range is ordered");
        Some(range)
    }

    #[must_use]
    pub fn selected_excerpt(&self) -> Option<SelectionExcerpt> {
        let count = self.row_count();
        if count == 0 {
            return None;
        }
        let range = self.selection_range().unwrap_or(self.cursor..=self.cursor);
        let start_row = (*range.start()).min(count - 1);
        let end_row = (*range.end()).min(count - 1);
        assert!(start_row <= end_row, "excerpt row range is ordered");
        let file_index = self.diff_file_at(start_row)?;
        assert!(
            file_index < self.patch.files.len(),
            "excerpt file index in bounds"
        );
        let file = &self.patch.files[file_index];
        let path = file.path().clone();
        let mut lines: Option<LineRange> = None;
        let mut selected = ExcerptBuffer::new();
        let mut has_code_line = false;
        let mut touched_hunks = BTreeSet::new();

        for row_index in start_row..=end_row {
            if self.diff_file_at(row_index) != Some(file_index) {
                break;
            }
            if selected.at_line_limit() {
                selected.mark_truncated();
                break;
            }
            match self.active_layout {
                ActiveLayout::Split => {
                    let row = &self.split.rows[row_index];
                    if let Some(number) = row.primary_line() {
                        match &mut lines {
                            Some(span) => span.extend(number),
                            None => lines = Some(LineRange::single(number)),
                        }
                    }
                    let SplitLine::Pair { left, right } = row else {
                        continue;
                    };
                    if let Some(left_file) = left.file_index() {
                        assert_eq!(
                            left_file, file_index,
                            "split pair left side belongs to selected file"
                        );
                    }
                    if let Some(right_file) = right.file_index() {
                        assert_eq!(
                            right_file, file_index,
                            "split pair right side belongs to selected file"
                        );
                    }
                    if let Some(hunk_index) = left.hunk_index() {
                        assert!(
                            hunk_index < file.hunks().len(),
                            "split left hunk index in bounds"
                        );
                        touched_hunks.insert(hunk_index);
                    }
                    if let Some(hunk_index) = right.hunk_index() {
                        assert!(
                            hunk_index < file.hunks().len(),
                            "split right hunk index in bounds"
                        );
                        touched_hunks.insert(hunk_index);
                    }
                    if matches!(left, LeftRow::Deleted { .. } | LeftRow::Context { .. })
                        || matches!(right, RightRow::Added { .. } | RightRow::Context { .. })
                    {
                        has_code_line = true;
                    }
                    let left_entry = match left {
                        LeftRow::Deleted { .. } => Some((Some('-'), left.text(&self.patch))),
                        LeftRow::Context { .. } if matches!(right, RightRow::Spacer) => {
                            Some((Some(' '), left.text(&self.patch)))
                        }
                        LeftRow::HunkHeader { text, .. } if matches!(right, RightRow::Spacer) => {
                            Some((None, text.as_str()))
                        }
                        LeftRow::Context { .. } | LeftRow::HunkHeader { .. } | LeftRow::Spacer => {
                            None
                        }
                    };
                    if let Some((prefix, text)) = left_entry
                        && !selected.try_push_line(prefix, text)
                    {
                        break;
                    }
                    let right_entry = match right {
                        RightRow::Added { .. } => Some((Some('+'), right.text(&self.patch))),
                        RightRow::Context { .. } => Some((Some(' '), right.text(&self.patch))),
                        RightRow::HunkHeader { text, .. } => Some((None, text.as_str())),
                        RightRow::Spacer => None,
                    };
                    if let Some((prefix, text)) = right_entry
                        && !selected.try_push_line(prefix, text)
                    {
                        break;
                    }
                }
                ActiveLayout::Unified => {
                    let row = &self.unified.rows[row_index];
                    if let Some(number) = row.primary_line() {
                        match &mut lines {
                            Some(span) => span.extend(number),
                            None => lines = Some(LineRange::single(number)),
                        }
                    }
                    if let Some(hunk_index) = row.hunk_index() {
                        assert!(
                            hunk_index < file.hunks().len(),
                            "unified hunk index in bounds"
                        );
                        touched_hunks.insert(hunk_index);
                    }
                    if matches!(
                        row,
                        UnifiedRow::Added { .. }
                            | UnifiedRow::Deleted { .. }
                            | UnifiedRow::Context { .. }
                    ) {
                        has_code_line = true;
                    }
                    let entry = match row {
                        UnifiedRow::Added { .. } => Some((Some('+'), row.text(&self.patch))),
                        UnifiedRow::Deleted { .. } => Some((Some('-'), row.text(&self.patch))),
                        UnifiedRow::Context { .. } => Some((Some(' '), row.text(&self.patch))),
                        UnifiedRow::HunkHeader { text, .. } => Some((None, text.as_str())),
                        UnifiedRow::FileHeader { .. } | UnifiedRow::BinaryNotice { .. } => None,
                    };
                    if let Some((prefix, text)) = entry
                        && !selected.try_push_line(prefix, text)
                    {
                        break;
                    }
                }
            }
        }

        let (final_snippet, enclosing_hunks) = if has_code_line {
            assert!(
                !touched_hunks.is_empty(),
                "code line selection touches at least one hunk"
            );
            let selected_snippet = selected.finish();
            let enclosing = format_hunks_excerpt(file.hunks(), touched_hunks.iter().copied());
            assert!(
                !enclosing.is_empty(),
                "enclosing hunks excerpt is non-empty"
            );
            let enclosing_hunks = (enclosing != selected_snippet).then_some(enclosing);
            (selected_snippet, enclosing_hunks)
        } else if !touched_hunks.is_empty() {
            (
                format_hunks_excerpt(file.hunks(), touched_hunks.iter().copied()),
                None,
            )
        } else {
            (format_file_excerpt(file), None)
        };

        Some(
            SelectionExcerpt::new(path, lines, final_snippet, enclosing_hunks)
                .expect("viewer builds a valid bounded selection excerpt"),
        )
    }

    pub fn start_search(&mut self) {
        self.search = SearchState::Editing {
            query: String::new(),
            found: None,
        };
    }

    pub fn search_push(&mut self, ch: char, viewport: usize) {
        let SearchState::Editing { query, .. } = &mut self.search else {
            return;
        };
        if query.len().saturating_add(ch.len_utf8()) > SEARCH_QUERY_BYTES_MAX {
            return;
        }
        query.push(ch);
        assert!(
            query.len() <= SEARCH_QUERY_BYTES_MAX,
            "search query stays within SEARCH_QUERY_BYTES_MAX"
        );
        self.recompute_search();
        if let Some(target) = self.search.found().map(SearchMatches::current) {
            self.jump_to_match(target, viewport);
        }
    }

    pub fn search_pop(&mut self, viewport: usize) {
        let SearchState::Editing { query, .. } = &mut self.search else {
            return;
        };
        query.pop();
        self.recompute_search();
        if let Some(target) = self.search.found().map(SearchMatches::current) {
            self.jump_to_match(target, viewport);
        }
    }

    pub fn search_commit(&mut self) {
        if let SearchState::Editing { query, found } =
            std::mem::replace(&mut self.search, SearchState::Idle)
            && !query.is_empty()
        {
            self.search = SearchState::Committed { query, found };
        }
    }

    pub fn search_cancel(&mut self) {
        self.search = SearchState::Idle;
    }

    pub fn search_next(&mut self, viewport: usize) {
        let target = match &mut self.search {
            SearchState::Editing {
                found: Some(matches),
                ..
            }
            | SearchState::Committed {
                found: Some(matches),
                ..
            } => Some(matches.step_next()),
            _ => None,
        };
        if let Some(target) = target {
            self.jump_to_match(target, viewport);
        }
    }

    pub fn search_prev(&mut self, viewport: usize) {
        let target = match &mut self.search {
            SearchState::Editing {
                found: Some(matches),
                ..
            }
            | SearchState::Committed {
                found: Some(matches),
                ..
            } => Some(matches.step_prev()),
            _ => None,
        };
        if let Some(target) = target {
            self.jump_to_match(target, viewport);
        }
    }

    fn recompute_search(&mut self) {
        let needle = match &self.search {
            SearchState::Idle => return,
            SearchState::Editing { query, .. } | SearchState::Committed { query, .. } => {
                if query.is_empty() {
                    if let SearchState::Editing { found, .. } = &mut self.search {
                        *found = None;
                    }
                    return;
                }
                query.clone()
            }
        };
        let mut found_matches = Vec::new();
        let count = self.row_count();
        for row_index in 0..count {
            if found_matches.len() >= SEARCH_MATCHES_MAX {
                break;
            }
            let column = match self.active_layout {
                ActiveLayout::Split => match &self.split.rows[row_index] {
                    SplitLine::FileHeader { file } => find_case_insensitive_range(
                        self.patch.files[*file].path().as_str(),
                        &needle,
                    ),
                    SplitLine::BinaryNotice { .. } => {
                        find_case_insensitive_range("Binary file changed", &needle)
                    }
                    SplitLine::Pair { left, right } => {
                        find_case_insensitive_range(right.text(&self.patch), &needle).or_else(
                            || find_case_insensitive_range(left.text(&self.patch), &needle),
                        )
                    }
                }
                .map(|range| range.start),
                ActiveLayout::Unified => find_case_insensitive_range(
                    self.unified.rows[row_index].text(&self.patch),
                    &needle,
                )
                .map(|range| range.start),
            };
            if let Some(column) = column {
                found_matches.push(SearchMatch {
                    row: row_index,
                    column,
                });
            }
        }
        assert!(
            found_matches.len() <= SEARCH_MATCHES_MAX,
            "search matches stay within SEARCH_MATCHES_MAX"
        );
        let built = SearchMatches::new(found_matches, self.cursor);
        match &mut self.search {
            SearchState::Idle => {}
            SearchState::Editing { found, .. } | SearchState::Committed { found, .. } => {
                *found = built;
            }
        }
    }

    fn jump_to_match(&mut self, found: SearchMatch, viewport: usize) {
        self.cursor = found.row;
        self.keep_visible(viewport);
        self.sync_tree_cursor();
    }

    pub fn keep_visible(&mut self, viewport: usize) {
        let count = self.row_count();
        if count == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self.cursor.min(count - 1);
        let viewport = viewport.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + viewport {
            self.scroll = self.cursor + 1 - viewport;
        }
        let max_scroll = count.saturating_sub(viewport);
        self.scroll = self.scroll.min(max_scroll);
        assert!(self.cursor < count, "cursor stays within row count");
        assert!(self.scroll <= max_scroll, "scroll stays within max_scroll");
    }

    fn clamp_cursor(&mut self) {
        let max = self.row_count().saturating_sub(1);
        self.cursor = self.cursor.min(max);
        self.scroll = self.scroll.min(self.cursor);
        assert!(self.scroll <= self.cursor, "scroll never passes cursor");
    }

    fn sync_tree_cursor(&mut self) {
        if let Some(file) = self.current_file()
            && let Some(index) = self.tree.index_of_file(file)
        {
            self.tree_cursor = index;
        }
    }

    fn cursor_location(&self) -> Option<(String, Option<u32>)> {
        let file_index = self.current_file()?;
        assert!(
            file_index < self.patch.files.len(),
            "cursor file index in bounds"
        );
        let path = self.patch.files[file_index].path().as_str().to_owned();
        let line = match self.active_layout {
            ActiveLayout::Split => self
                .split
                .rows
                .get(self.cursor)
                .and_then(SplitLine::primary_line),
            ActiveLayout::Unified => self
                .unified
                .rows
                .get(self.cursor)
                .and_then(UnifiedRow::primary_line),
        };
        Some((path, line))
    }

    fn restore_cursor(&mut self, path: &str, line: Option<u32>) {
        let Some(file_index) = self
            .patch
            .files
            .iter()
            .position(|file| file.path().as_str() == path)
        else {
            self.clamp_cursor();
            return;
        };
        let files = match self.active_layout {
            ActiveLayout::Split => &self.split.files,
            ActiveLayout::Unified => &self.unified.files,
        };
        let Some(span) = files.iter().find(|span| span.file == file_index) else {
            self.clamp_cursor();
            return;
        };
        if let Some(target_line) = line {
            let found = (span.start_row..span.end_row).find(|&index| match self.active_layout {
                ActiveLayout::Split => self.split.rows[index].has_line(target_line),
                ActiveLayout::Unified => {
                    self.unified.rows[index].primary_line() == Some(target_line)
                }
            });
            if let Some(row) = found {
                self.cursor = row;
                self.scroll = self.scroll.min(row);
                return;
            }
        }
        self.cursor = span.start_row;
        self.scroll = span.start_row;
    }
}

struct ExcerptBuffer {
    text: String,
    lines_count: usize,
    truncated: bool,
}

impl ExcerptBuffer {
    fn new() -> ExcerptBuffer {
        ExcerptBuffer {
            text: String::new(),
            lines_count: 0,
            truncated: false,
        }
    }

    const fn at_line_limit(&self) -> bool {
        self.lines_count >= EXCERPT_LINES_MAX
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    fn try_push_line(&mut self, prefix: Option<char>, line: &str) -> bool {
        if self.lines_count < EXCERPT_LINES_MAX {
            let pushed = push_excerpt_line(&mut self.text, prefix, line, EXCERPT_CONTENT_BYTES_MAX);
            if pushed {
                self.lines_count += 1;
                assert!(
                    self.lines_count <= EXCERPT_LINES_MAX,
                    "excerpt line count stays within EXCERPT_LINES_MAX"
                );
                return true;
            }
        }
        self.truncated = true;
        false
    }

    fn finish(mut self) -> String {
        assert!(
            self.lines_count <= EXCERPT_LINES_MAX,
            "excerpt line count stays within EXCERPT_LINES_MAX"
        );
        if self.truncated {
            if !self.text.is_empty() {
                self.text.push('\n');
            }
            self.text.push_str(EXCERPT_TRUNCATED_MARKER);
        }
        assert!(
            self.text.len() <= EXCERPT_BYTES_MAX,
            "formatted excerpt stays within EXCERPT_BYTES_MAX"
        );
        self.text
    }
}

fn strip_nul(text: String) -> String {
    let cleaned = if text.as_bytes().contains(&0) {
        text.replace('\0', "")
    } else {
        text
    };
    assert!(
        !cleaned.as_bytes().contains(&0),
        "sanitized excerpt text contains no NUL bytes"
    );
    cleaned
}

fn format_file_excerpt(file: &FilePatch) -> String {
    let excerpt = match &file.body {
        FileBody::Binary => match &file.change {
            FileChange::Added(_) => "Binary file added".to_owned(),
            FileChange::Deleted(_) => "Binary file deleted".to_owned(),
            FileChange::Modified(_) => "Binary file changed".to_owned(),
            FileChange::Renamed { old, .. } => format!("Binary file renamed from {old}"),
        },
        FileBody::Text(hunks) if hunks.is_empty() => match &file.change {
            FileChange::Added(_) => "New empty file (no diff hunks)".to_owned(),
            FileChange::Deleted(_) => "Deleted empty file (no diff hunks)".to_owned(),
            FileChange::Renamed { old, .. } => {
                format!("Renamed from {old} with no content changes")
            }
            FileChange::Modified(_) => "File changed with no text diff hunks".to_owned(),
        },
        FileBody::Text(hunks) => format_hunks_excerpt(hunks, 0..hunks.len()),
    };
    assert!(!excerpt.is_empty(), "file excerpt is non-empty");
    assert!(
        excerpt.len() <= EXCERPT_BYTES_MAX,
        "file excerpt stays within EXCERPT_BYTES_MAX"
    );
    excerpt
}

fn format_hunks_excerpt<I>(hunks: &[Hunk], indices: I) -> String
where
    I: IntoIterator<Item = usize>,
{
    let mut excerpt = ExcerptBuffer::new();

    'hunks: for hunk_index in indices {
        assert!(
            hunk_index < hunks.len(),
            "hunk index stays within file hunks"
        );
        let hunk = &hunks[hunk_index];
        let header = hunk.header_text();
        if !excerpt.try_push_line(None, &header) {
            break 'hunks;
        }

        for block in &hunk.blocks {
            match block {
                Block::Context(lines) => {
                    for context_line in lines.iter() {
                        if !excerpt.try_push_line(Some(' '), &context_line.text) {
                            break 'hunks;
                        }
                    }
                }
                Block::Changed(changed) => {
                    for deleted_line in changed.deleted() {
                        if !excerpt.try_push_line(Some('-'), &deleted_line.text) {
                            break 'hunks;
                        }
                    }
                    for added_line in changed.added() {
                        if !excerpt.try_push_line(Some('+'), &added_line.text) {
                            break 'hunks;
                        }
                    }
                }
            }
        }
    }

    excerpt.finish()
}

fn push_excerpt_line(
    out: &mut String,
    prefix: Option<char>,
    text: &str,
    byte_budget: usize,
) -> bool {
    let separator = usize::from(!out.is_empty());
    let prefix_len = prefix.map_or(0, char::len_utf8);
    let needed = separator + prefix_len + text.len();
    if out.len().saturating_add(needed) > byte_budget {
        return false;
    }
    if separator > 0 {
        out.push('\n');
    }
    if let Some(ch) = prefix {
        out.push(ch);
    }
    out.push_str(text);
    assert!(
        out.len() <= byte_budget,
        "excerpt line respects byte budget"
    );
    true
}

#[must_use]
pub fn find_case_insensitive_range(haystack: &str, needle: &str) -> Option<Range<usize>> {
    if needle.is_empty() || haystack.is_empty() {
        return None;
    }
    if needle.is_ascii() {
        let needle_bytes = needle.as_bytes();
        let hay_bytes = haystack.as_bytes();
        if needle_bytes.len() > hay_bytes.len() {
            return None;
        }
        for start in 0..=hay_bytes.len() - needle_bytes.len() {
            if hay_bytes[start..start + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
                && haystack.is_char_boundary(start)
                && haystack.is_char_boundary(start + needle_bytes.len())
            {
                let end = start + needle_bytes.len();
                assert!(end <= haystack.len(), "ASCII match stays in haystack");
                return Some(start..end);
            }
        }
        return None;
    }
    let needle_lower: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle_lower.is_empty() {
        return None;
    }
    let hay_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    for start_idx in 0..hay_chars.len() {
        let mut matched = 0usize;
        let mut cursor = start_idx;
        let mut ok = true;
        while matched < needle_lower.len() && cursor < hay_chars.len() {
            let mut folded = hay_chars[cursor].1.to_lowercase();
            let mut step_matched = true;
            for ch in folded.by_ref() {
                if matched < needle_lower.len() && ch == needle_lower[matched] {
                    matched += 1;
                } else {
                    step_matched = false;
                    break;
                }
            }
            if !step_matched {
                ok = false;
                break;
            }
            cursor += 1;
        }
        if ok && matched == needle_lower.len() {
            let start = hay_chars[start_idx].0;
            let end = hay_chars
                .get(cursor)
                .map_or(haystack.len(), |(pos, _)| *pos);
            assert!(start < end, "Unicode match is non-empty");
            assert!(end <= haystack.len(), "Unicode match stays in haystack");
            assert!(
                haystack.is_char_boundary(start),
                "Unicode match start aligned"
            );
            assert!(haystack.is_char_boundary(end), "Unicode match end aligned");
            return Some(start..end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 fn alpha() {
-    old_call();
+    new_call();
 }
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,2 +1,3 @@
 fn beta() {
+    added_line();
 }
";

    #[test]
    fn navigation_search_selection_and_reload_preserve_state() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        assert!(viewer.sidebar().is_open());
        viewer.sync_layout(120);
        assert!(viewer.is_split());
        viewer.sync_layout(60);
        assert!(!viewer.is_split());
        viewer.sync_layout(120);
        assert!(viewer.is_split());

        viewer.next_file(20);
        assert_eq!(viewer.current_file(), Some(1));
        viewer.toggle_viewed();
        assert!(viewer.is_viewed(1));

        viewer.start_search();
        for ch in "NEW_CALL".chars() {
            viewer.search_push(ch, 20);
        }
        viewer.search_commit();
        assert_eq!(viewer.search().found().map(|m| m.count().get()), Some(1));
        assert_eq!(viewer.current_file(), Some(0));

        viewer.toggle_select();
        let excerpt = viewer.selected_excerpt().unwrap();
        assert_eq!(excerpt.path(), "src/a.rs");
        assert!(excerpt.snippet().contains("new_call"));
        assert!(excerpt.lines().is_some_and(|l| l.start() <= l.end()));

        let reloaded = Patch::parse(SAMPLE.as_bytes());
        viewer.reload(reloaded, "main".to_owned(), "feat".to_owned());
        assert!(viewer.is_viewed(1));
        assert_eq!(viewer.current_file(), Some(0));
    }

    #[test]
    fn resize_across_threshold_reanchors_cursor_clears_selection_and_recomputes_search() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        viewer.sync_layout(120);
        assert!(viewer.is_split());

        viewer.start_search();
        for ch in "added_line".chars() {
            viewer.search_push(ch, 20);
        }
        viewer.search_commit();
        let split_match_row = viewer.search().found().unwrap().current().row;
        viewer.toggle_select();
        assert!(viewer.is_selecting());

        viewer.sync_layout(60);
        assert!(!viewer.is_split());
        assert!(
            !viewer.is_selecting(),
            "layout switch clears stale selection"
        );
        let unified_match_row = viewer.search().found().unwrap().current().row;
        assert_ne!(unified_match_row, split_match_row);
        assert_eq!(viewer.current_file(), Some(1));
        assert!(
            viewer.unified().rows[unified_match_row]
                .text(viewer.patch())
                .contains("added_line")
        );

        viewer.toggle_layout(60);
        assert!(viewer.is_split());
        viewer.sync_layout(SPLIT_HARD_MIN_WIDTH - 1);
        assert!(
            !viewer.is_split(),
            "narrow width below SPLIT_HARD_MIN_WIDTH forces unified layout"
        );
    }

    #[test]
    fn sidebar_focus_cannot_target_hidden_or_empty_tree_and_empty_search_commits_idle() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        viewer.toggle_focus(false);
        assert!(!viewer.sidebar().is_focused());

        viewer.toggle_focus(true);
        assert!(viewer.sidebar().is_focused());
        viewer.sync_tree_availability(false);
        assert!(viewer.sidebar().is_open());
        assert!(!viewer.sidebar().is_focused());

        viewer.start_search();
        assert!(viewer.search().is_editing());
        viewer.search_commit();
        assert_eq!(*viewer.search(), SearchState::Idle);
    }

    #[test]
    fn excerpt_is_bounded_and_search_query_is_capped() {
        use std::fmt::Write as _;

        let mut huge = String::new();
        for index in 0..500 {
            let _ = writeln!(huge, "let line_{index} = {};", "x".repeat(80));
        }
        let mut patch = Patch::parse(b"");
        patch.push_untracked(FilePath::parse("src/huge.rs").unwrap(), &huge);
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        let file_header_excerpt = viewer.selected_excerpt().unwrap();
        assert!(file_header_excerpt.snippet().len() <= EXCERPT_BYTES_MAX);
        assert!(
            file_header_excerpt
                .snippet()
                .ends_with(EXCERPT_TRUNCATED_MARKER)
        );
        assert_eq!(file_header_excerpt.enclosing_hunks(), None);

        viewer.next_hunk(20);
        let hunk_header_excerpt = viewer.selected_excerpt().unwrap();
        assert!(hunk_header_excerpt.snippet().len() <= EXCERPT_BYTES_MAX);
        assert!(
            hunk_header_excerpt
                .snippet()
                .ends_with(EXCERPT_TRUNCATED_MARKER)
        );
        assert_eq!(hunk_header_excerpt.enclosing_hunks(), None);

        viewer.top(20);
        viewer.toggle_select();
        viewer.bottom(20);
        let excerpt = viewer.selected_excerpt().unwrap();
        assert!(excerpt.snippet().len() <= EXCERPT_BYTES_MAX);
        assert!(excerpt.snippet().ends_with(EXCERPT_TRUNCATED_MARKER));

        viewer.start_search();
        for _ in 0..SEARCH_QUERY_BYTES_MAX + 50 {
            viewer.search_push('a', 20);
        }
        assert_eq!(
            viewer.search().query().map(str::len),
            Some(SEARCH_QUERY_BYTES_MAX)
        );
    }

    #[test]
    fn case_insensitive_search_preserves_original_utf8_byte_offsets() {
        let text = "İfoo Bar";
        let range = find_case_insensitive_range(text, "bar").unwrap();
        assert_eq!(&text[range], "Bar");

        let unicode = "İstanbul";
        let range = find_case_insensitive_range(unicode, "bul").unwrap();
        assert_eq!(&unicode[range], "bul");

        let dotted = "prefix İ suffix";
        let range = find_case_insensitive_range(dotted, "İ").unwrap();
        assert_eq!(&dotted[range], "İ");
    }

    #[test]
    fn excerpt_uses_diff_cursor_file_stays_within_one_file_and_orders_split_lines() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        viewer.sync_layout(120);
        viewer.move_down(3, 20);
        viewer.toggle_focus(true);
        viewer.bottom(20);
        assert_eq!(viewer.current_file(), Some(1));
        let excerpt = viewer.selected_excerpt().unwrap();
        assert_eq!(excerpt.path(), "src/a.rs");
        assert_eq!(excerpt.snippet(), "-    old_call();\n+    new_call();");
        assert_eq!(
            excerpt.enclosing_hunks(),
            Some("@@ -1,3 +1,3 @@\n fn alpha() {\n-    old_call();\n+    new_call();\n }")
        );

        viewer.unfocus_sidebar();
        viewer.toggle_select();
        viewer.bottom(20);
        let across = viewer.selected_excerpt().unwrap();
        assert_eq!(across.path(), "src/a.rs");
        assert!(!across.snippet().contains("added_line"));

        let drifted = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ a/src/a.rs
@@ -1,4 +1,2 @@
-one
-two
-three
+one_new
 tail_ctx
";
        let mut split_viewer = Viewer::new(
            Patch::parse(drifted.as_bytes()),
            "main".to_owned(),
            "feat".to_owned(),
        );
        split_viewer.sync_layout(120);
        split_viewer.bottom(20);
        let tail = split_viewer.selected_excerpt().unwrap();
        assert!(tail.snippet().contains(" tail_ctx"), "{:?}", tail.snippet());
    }

    #[test]
    fn excerpt_expands_headers_binary_empty_and_dir_click_preserves_focus() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        let mut viewer = Viewer::new(patch, "main".to_owned(), "feat".to_owned());
        viewer.sync_layout(120);

        for width in [120, 60] {
            viewer.sync_layout(width);
            viewer.top(20);
            let on_file_header = viewer.selected_excerpt().unwrap();
            assert_eq!(on_file_header.path(), "src/a.rs");
            assert_eq!(on_file_header.lines(), None);
            assert_eq!(on_file_header.enclosing_hunks(), None);
            assert!(
                on_file_header.snippet().contains("-    old_call();"),
                "{on_file_header:?}"
            );
            assert!(
                on_file_header.snippet().contains("+    new_call();"),
                "{on_file_header:?}"
            );

            viewer.next_hunk(20);
            let on_hunk_header = viewer.selected_excerpt().unwrap();
            assert_eq!(on_hunk_header.path(), "src/a.rs");
            assert_eq!(on_hunk_header.enclosing_hunks(), None);
            assert!(
                on_hunk_header.snippet().contains("@@ -1,3 +1,3 @@"),
                "{on_hunk_header:?}"
            );
            assert!(
                on_hunk_header.snippet().contains("-    old_call();"),
                "{on_hunk_header:?}"
            );
            assert!(
                on_hunk_header.snippet().contains("+    new_call();"),
                "{on_hunk_header:?}"
            );
        }

        viewer.sync_layout(60);
        viewer.top(20);
        viewer.next_hunk(20);
        viewer.toggle_select();
        viewer.move_down(4, 20);
        let full_hunk = viewer.selected_excerpt().unwrap();
        assert_eq!(
            full_hunk.enclosing_hunks(),
            None,
            "identical enclosing hunk is not duplicated"
        );
        viewer.clear_select();

        let special = "\
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
diff --git a/old.png b/new.png
rename from old.png
rename to new.png
Binary files a/old.png and b/new.png differ
diff --git a/empty.txt b/empty.txt
new file mode 100644
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
";
        let mut special_viewer = Viewer::new(
            Patch::parse(special.as_bytes()),
            "main".to_owned(),
            "feat".to_owned(),
        );
        for width in [120, 60] {
            special_viewer.sync_layout(width);
            for row in 0..special_viewer.row_count() {
                special_viewer.click_diff_row(row, 20);
                let excerpt = special_viewer.selected_excerpt().unwrap();
                assert!(
                    !excerpt.snippet().is_empty(),
                    "width {width} row {row}: {excerpt:?}"
                );
            }
        }

        let multi_hunk = "\
diff --git a/src/multi.rs b/src/multi.rs
--- a/src/multi.rs
+++ a/src/multi.rs
@@ -1,3 +1,3 @@
 fn first() {
-    one();
+    two();
 }
@@ -20,3 +20,3 @@
 fn second() {
-    three();
+    four();
 }
";
        let mut multi_viewer = Viewer::new(
            Patch::parse(multi_hunk.as_bytes()),
            "main".to_owned(),
            "feat".to_owned(),
        );
        for width in [120, 60] {
            multi_viewer.sync_layout(width);
            multi_viewer.top(20);
            multi_viewer.move_down(3, 20);
            multi_viewer.toggle_select();
            multi_viewer.move_down(5, 20);
            let across_hunks = multi_viewer.selected_excerpt().unwrap();
            let enclosing = across_hunks
                .enclosing_hunks()
                .expect("partial multi-hunk selection includes both enclosing hunks");
            assert!(enclosing.contains("@@ -1,3 +1,3 @@"), "{enclosing}");
            assert!(enclosing.contains(" fn first() {"), "{enclosing}");
            assert!(enclosing.contains("@@ -20,3 +20,3 @@"), "{enclosing}");
            assert!(enclosing.contains(" fn second() {"), "{enclosing}");
            multi_viewer.clear_select();
        }

        let sample_path = FilePath::parse("src/a.rs").unwrap();
        assert_eq!(
            SelectionExcerpt::new(sample_path.clone(), None, String::new(), None),
            None
        );
        assert_eq!(
            SelectionExcerpt::new(
                sample_path.clone(),
                None,
                "\0".to_owned(),
                Some("@@ -1 +1 @@\n+x".to_owned()),
            ),
            None
        );
        assert_eq!(
            SelectionExcerpt::new(
                sample_path.clone(),
                None,
                "x".repeat(EXCERPT_BYTES_MAX + 1),
                None,
            ),
            None
        );
        assert_eq!(
            SelectionExcerpt::new(
                sample_path.clone(),
                None,
                "+ok".to_owned(),
                Some("x".repeat(EXCERPT_BYTES_MAX + 1)),
            ),
            None
        );
        let deduped = SelectionExcerpt::new(
            sample_path,
            None,
            "+ok\0".to_owned(),
            Some("+ok\0".to_owned()),
        )
        .unwrap();
        assert_eq!(deduped.snippet(), "+ok");
        assert_eq!(deduped.enclosing_hunks(), None);

        assert!(viewer.sidebar().is_open());
        assert!(!viewer.sidebar().is_focused());
        let dir_index = viewer
            .tree()
            .entries
            .iter()
            .position(|entry| matches!(entry.node, TreeNode::Dir { .. }))
            .expect("SAMPLE has a src directory node");
        viewer.activate_tree_index(dir_index, 20);
        assert!(
            !viewer.sidebar().is_focused(),
            "clicking a directory folds it without trapping focus in the file tree"
        );
    }
}
