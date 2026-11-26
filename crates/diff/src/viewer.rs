use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use crate::layout::{LeftRow, RightRow, SplitLayout, SplitLine, UnifiedLayout, UnifiedRow};
use crate::patch::{FilePath, Patch};
use crate::ribbon::GUTTER_COLS;
use crate::tree::{FileTree, TreeNode};

pub const LINE_NUMBER_COLS: u16 = 4;

pub const SPLIT_MIN_WIDTH: u16 = 88;

pub const SPLIT_HARD_MIN_WIDTH: u16 = LINE_NUMBER_COLS * 2 + GUTTER_COLS + 7;

pub const SEARCH_QUERY_BYTES_MAX: usize = 256;

const SEARCH_MATCHES_MAX: usize = 4096;

pub const EXCERPT_LINES_MAX: usize = 200;

pub const EXCERPT_BYTES_MAX: usize = 16 * 1024;

const EXCERPT_TRUNCATED_MARKER: &str = "... [truncated]";

const _: () = assert!(SPLIT_HARD_MIN_WIDTH <= SPLIT_MIN_WIDTH);
const _: () = assert!(SEARCH_QUERY_BYTES_MAX > 0);
const _: () = assert!(SEARCH_MATCHES_MAX > 0);
const _: () = assert!(EXCERPT_LINES_MAX > 0);
const _: () = assert!(EXCERPT_BYTES_MAX >= EXCERPT_TRUNCATED_MARKER.len() + 64);

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
    pub path: FilePath,
    pub lines: Option<LineRange>,
    pub snippet: String,
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
        assert_eq!(split.files.len(), patch.files.len(), "split layout covers all files");
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
        assert!(file_index < self.patch.files.len(), "current file index in bounds");
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
    pub fn current_file(&self) -> Option<usize> {
        if self.sidebar.is_focused()
            && let Some(entry) = self.tree.entries.get(self.tree_cursor)
            && let TreeNode::File { file, .. } = entry.node
        {
            return Some(file);
        }
        match self.active_layout {
            ActiveLayout::Split => self
                .split
                .rows
                .get(self.cursor)
                .and_then(SplitLine::file_index)
                .or_else(|| {
                    self.split
                        .files
                        .iter()
                        .find(|span| self.cursor >= span.start_row && self.cursor < span.end_row)
                        .map(|span| span.file)
                }),
            ActiveLayout::Unified => self
                .unified
                .rows
                .get(self.cursor)
                .map(UnifiedRow::file_index),
        }
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
            assert!(self.cursor < count, "cursor remains within rows after scroll");
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
        if index >= self.tree.entries.len() {
            return;
        }
        self.sidebar = Sidebar::Open { focused: true };
        self.tree_cursor = index;
        self.activate_tree_entry(viewport);
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
        let file_index = self.current_file()?;
        assert!(file_index < self.patch.files.len(), "excerpt file index in bounds");
        let path = self.patch.files[file_index].path().clone();
        let mut lines: Option<LineRange> = None;
        let mut snippet = String::new();
        let mut emitted_lines = 0usize;
        let mut truncated = false;
        let byte_budget = EXCERPT_BYTES_MAX - EXCERPT_TRUNCATED_MARKER.len() - 1;

        for row_index in start_row..=end_row {
            if emitted_lines >= EXCERPT_LINES_MAX {
                truncated = true;
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
                    let right_entry = match right {
                        RightRow::Added { .. } => Some((Some('+'), right.text(&self.patch))),
                        RightRow::Context { .. } => Some((Some(' '), right.text(&self.patch))),
                        RightRow::HunkHeader { text, .. } => Some((None, text.as_str())),
                        RightRow::Spacer => None,
                    };
                    if let Some((prefix, text)) = right_entry {
                        if !push_excerpt_line(&mut snippet, prefix, text, byte_budget) {
                            truncated = true;
                            break;
                        }
                        emitted_lines += 1;
                    }
                    if let LeftRow::Deleted { .. } = left {
                        if emitted_lines >= EXCERPT_LINES_MAX
                            || !push_excerpt_line(
                                &mut snippet,
                                Some('-'),
                                left.text(&self.patch),
                                byte_budget,
                            )
                        {
                            truncated = true;
                            break;
                        }
                        emitted_lines += 1;
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
                    let entry = match row {
                        UnifiedRow::Added { .. } => Some((Some('+'), row.text(&self.patch))),
                        UnifiedRow::Deleted { .. } => Some((Some('-'), row.text(&self.patch))),
                        UnifiedRow::Context { .. } => Some((Some(' '), row.text(&self.patch))),
                        UnifiedRow::HunkHeader { text, .. } => Some((None, text.as_str())),
                        UnifiedRow::FileHeader { .. } | UnifiedRow::BinaryNotice { .. } => None,
                    };
                    if let Some((prefix, text)) = entry {
                        if !push_excerpt_line(&mut snippet, prefix, text, byte_budget) {
                            truncated = true;
                            break;
                        }
                        emitted_lines += 1;
                    }
                }
            }
        }
        if truncated {
            if !snippet.is_empty() {
                snippet.push('\n');
            }
            snippet.push_str(EXCERPT_TRUNCATED_MARKER);
        }
        assert!(
            snippet.len() <= EXCERPT_BYTES_MAX,
            "selection excerpt stays within EXCERPT_BYTES_MAX"
        );
        Some(SelectionExcerpt {
            path,
            lines,
            snippet,
        })
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
            SearchState::Editing { found: Some(matches), .. }
            | SearchState::Committed { found: Some(matches), .. } => Some(matches.step_next()),
            _ => None,
        };
        if let Some(target) = target {
            self.jump_to_match(target, viewport);
        }
    }

    pub fn search_prev(&mut self, viewport: usize) {
        let target = match &mut self.search {
            SearchState::Editing { found: Some(matches), .. }
            | SearchState::Committed { found: Some(matches), .. } => Some(matches.step_prev()),
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
        assert!(file_index < self.patch.files.len(), "cursor file index in bounds");
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
                ActiveLayout::Unified => self.unified.rows[index].primary_line() == Some(target_line),
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

fn push_excerpt_line(out: &mut String, prefix: Option<char>, text: &str, byte_budget: usize) -> bool {
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
    assert!(out.len() <= byte_budget, "excerpt line respects byte budget");
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
            let end = hay_chars.get(cursor).map_or(haystack.len(), |(pos, _)| *pos);
            assert!(start < end, "Unicode match is non-empty");
            assert!(end <= haystack.len(), "Unicode match stays in haystack");
            assert!(haystack.is_char_boundary(start), "Unicode match start aligned");
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
        assert_eq!(excerpt.path, "src/a.rs");
        assert!(excerpt.snippet.contains("new_call"));
        assert!(excerpt.lines.is_some_and(|l| l.start() <= l.end()));

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
        assert!(!viewer.is_selecting(), "layout switch clears stale selection");
        let unified_match_row = viewer.search().found().unwrap().current().row;
        assert_ne!(unified_match_row, split_match_row);
        assert_eq!(viewer.current_file(), Some(1));
        assert!(viewer.unified().rows[unified_match_row]
            .text(viewer.patch())
            .contains("added_line"));

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
        viewer.toggle_select();
        viewer.bottom(20);
        let excerpt = viewer.selected_excerpt().unwrap();
        assert!(excerpt.snippet.len() <= EXCERPT_BYTES_MAX);
        assert!(excerpt.snippet.ends_with(EXCERPT_TRUNCATED_MARKER));

        viewer.start_search();
        for _ in 0..SEARCH_QUERY_BYTES_MAX + 50 {
            viewer.search_push('a', 20);
        }
        assert_eq!(viewer.search().query().map(str::len), Some(SEARCH_QUERY_BYTES_MAX));
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
}
