use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zellij_tile::prelude::Text;

const TOP_LEFT: char = '╭';
const TOP_RIGHT: char = '╮';
const BOTTOM_LEFT: char = '╰';
const BOTTOM_RIGHT: char = '╯';
const HORIZONTAL: char = '─';
pub const VERTICAL: char = '│';

const MIN_TITLE: usize = 4;

pub const PAD: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub fn inner_y(&self) -> usize {
        self.y + 1
    }

    pub fn bottom_y(&self) -> usize {
        self.y + self.height.saturating_sub(1)
    }

    pub fn inner_height(&self) -> usize {
        self.height.saturating_sub(2)
    }

    pub fn inner_width(&self) -> usize {
        self.width.saturating_sub(PAD * 2)
    }

    pub fn top(&self, title: &str, right: &str) -> Border {
        let Some(mut rule) = self.width.checked_sub(2) else {
            return Border::rule(HORIZONTAL.to_string().repeat(self.width));
        };
        let mut line = String::from(TOP_LEFT);
        let mut tail = String::new();
        let mut title_at = None;

        let mut has_right = false;
        if !right.is_empty() && rule >= right.width() + 4 {
            tail = format!(" {} {}", right, HORIZONTAL);
            rule -= tail.width();
            has_right = true;
        }

        if !title.is_empty() && rule >= MIN_TITLE + 4 {
            let title = truncate(title, rule - 4);
            line.push(HORIZONTAL);
            line.push(' ');
            let start = line.chars().count();
            line.push_str(&title);
            title_at = Some(start..line.chars().count());
            line.push(' ');
            rule -= title.width() + 3;
        }
        line.extend(std::iter::repeat_n(HORIZONTAL, rule));
        let right_start = line.chars().count() + 1;
        line.push_str(&tail);
        line.push(TOP_RIGHT);
        Border {
            line,
            title: title_at,
            right: has_right.then(|| right_start..right_start + right.chars().count()),
        }
    }

    pub fn bottom(&self) -> String {
        let Some(rule) = self.width.checked_sub(2) else {
            return HORIZONTAL.to_string().repeat(self.width);
        };
        let mut line = String::from(BOTTOM_LEFT);
        line.extend(std::iter::repeat_n(HORIZONTAL, rule));
        line.push(BOTTOM_RIGHT);
        line
    }

    pub fn blank(&self) -> String {
        " ".repeat(self.inner_width() + 2)
    }

}

pub struct Border {
    pub line: String,
    pub title: Option<Range<usize>>,
    pub right: Option<Range<usize>>,
}

impl Border {
    fn rule(line: String) -> Self {
        Self { line, title: None, right: None }
    }

    pub fn rule_indices(&self) -> Vec<usize> {
        let labelled = |i: &usize| {
            self.title.as_ref().is_some_and(|r| r.contains(i))
                || self.right.as_ref().is_some_and(|r| r.contains(i))
        };
        (0..self.line.chars().count()).filter(|i| !labelled(i)).collect()
    }
}

pub struct Line {
    text: String,
    chars: usize,
    columns: usize,
    styles: Vec<Style>,
}

enum Style {
    Level(usize, Range<usize>),
    Hits(usize, Vec<usize>),
    Error(Range<usize>),
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl Line {
    pub fn new() -> Self {
        Self { text: String::new(), chars: 0, columns: 0, styles: Vec::new() }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn push(&mut self, text: &str, level: usize) {
        let range = self.raw(text);
        self.styles.push(Style::Level(level, range));
    }

    pub fn push_hits(&mut self, text: &str, level: usize, accent: usize, hits: &[usize]) {
        let range = self.raw(text);
        let start = range.start;
        self.styles.push(Style::Level(level, range));
        self.styles.push(Style::Hits(accent, hits.iter().map(|i| start + i).collect()));
    }

    pub fn push_error(&mut self, text: &str) {
        let range = self.raw(text);
        self.styles.push(Style::Error(range));
    }

    pub fn gap(&mut self, n: usize) {
        self.text.extend(std::iter::repeat_n(' ', n));
        self.chars += n;
        self.columns += n;
    }

    pub fn pad_to(&mut self, columns: usize) {
        self.gap(columns.saturating_sub(self.columns));
    }

    fn raw(&mut self, text: &str) -> Range<usize> {
        let start = self.chars;
        self.text.push_str(text);
        self.chars += text.chars().count();
        self.columns += text.width();
        start..self.chars
    }

    pub fn finish(self, inner_width: usize) -> Text {
        let Line { mut text, mut chars, mut columns, styles } = self;
        if columns > inner_width {
            text = truncate(&text, inner_width);
            chars = text.chars().count();
            columns = text.width();
        }
        let visible = chars;

        let mut line = String::with_capacity(text.len() + inner_width - columns + 2);
        line.push(' ');
        line.push_str(&text);
        line.extend(std::iter::repeat_n(' ', inner_width - columns));
        line.push(' ');

        let shift = |i: usize| i + 1;
        let clamp = |range: Range<usize>| {
            let start = range.start.min(visible);
            let end = range.end.min(visible);
            (start < end).then(|| shift(start)..shift(end))
        };

        // Every position no style covers is a space — padding, a gap, or the
        // border margin — so it needs no index list of its own to look right.
        let mut styled: Vec<usize> = Vec::new();
        for style in &styles {
            match style {
                Style::Level(_, range) | Style::Error(range) => {
                    if let Some(range) = clamp(range.clone()) {
                        styled.extend(range);
                    }
                },
                Style::Hits(_, indices) => {
                    styled.extend(indices.iter().filter(|i| **i < visible).map(|i| shift(*i)));
                },
            }
        }

        let mut text = Text::new(&line).unbold_indices(styled);
        for style in styles {
            text = match style {
                Style::Level(level, range) => match clamp(range) {
                    Some(range) => text.color_range(level, range),
                    None => text,
                },
                Style::Hits(level, indices) => text.color_indices(
                    level,
                    indices.into_iter().filter(|i| *i < visible).map(shift).collect(),
                ),
                Style::Error(range) => match clamp(range) {
                    Some(range) => text.error_color_range(range),
                    None => text,
                },
            };
        }
        text
    }
}

pub struct Screen {
    pub results: Option<Rect>,
    pub preview: Option<Rect>,
    pub full: Option<Rect>,
    pub input: Rect,
    pub bordered: bool,
    pub help_y: Option<usize>,
}

const INPUT_HEIGHT: usize = 3;

const MIN_RESULTS: usize = 3;

const MIN_HALF: usize = 26;

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        let help_y = (rows > INPUT_HEIGHT).then(|| rows - 1);
        let bordered = rows >= INPUT_HEIGHT;
        let height = if bordered { INPUT_HEIGHT } else { 1 };
        let input_y = rows.saturating_sub(usize::from(help_y.is_some()) + height);
        let input = Rect { x: 0, y: input_y, width: cols, height };
        let full = (input_y >= MIN_RESULTS)
            .then_some(Rect { x: 0, y: 0, width: cols, height: input_y });
        let split = full.filter(|rect| rect.width >= MIN_HALF * 2).map(|rect| {
            let left = rect.width / 2;
            (
                Rect { width: left, ..rect },
                Rect { x: left, width: rect.width - left, ..rect },
            )
        });
        let (results, preview) = match split {
            Some((results, preview)) => (Some(results), Some(preview)),
            None => (full, None),
        };
        Self { results, preview, full, input, bordered, help_y }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Block {
    pub y: usize,
    pub notes: usize,
    pub rows: usize,
}

pub fn anchor(rect: &Rect, notes: usize, rows: usize) -> Block {
    let height = rect.inner_height();
    let notes = notes.min(height);
    let rows = rows.min(height - notes);
    Block { y: rect.inner_y() + (height - notes - rows), notes, rows }
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

pub fn truncate_left(text: &str, max: usize) -> (String, usize) {
    if text.width() <= max {
        return (text.to_string(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut width = 0;
    let mut kept = 0;
    for ch in chars.iter().rev() {
        let w = ch.width().unwrap_or(0);
        if width + w > max.saturating_sub(1) {
            break;
        }
        width += w;
        kept += 1;
    }
    let dropped = chars.len() - kept;
    let mut out = String::from("…");
    out.extend(chars[dropped..].iter());
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tall_pane_gets_both_boxes() {
        let screen = Screen::new(30, 80);
        let full = screen.full.expect("a 30-row pane has room for a results box");
        assert_eq!(full, Rect { x: 0, y: 0, width: 80, height: 26 });
        assert_eq!(screen.input, Rect { x: 0, y: 26, width: 80, height: 3 });
        assert_eq!(screen.help_y, Some(29));
        assert!(screen.bordered);
        assert_eq!(full.bottom_y() + 1, screen.input.y);
        assert_eq!(screen.input.bottom_y() + 1, screen.help_y.unwrap());
    }

    #[test]
    fn a_wide_pane_puts_a_preview_beside_the_list() {
        let screen = Screen::new(30, 80);
        let results = screen.results.expect("80 columns is wide enough to split");
        let preview = screen.preview.expect("80 columns is wide enough to split");
        assert_eq!(results, Rect { x: 0, y: 0, width: 40, height: 26 });
        assert_eq!(preview, Rect { x: 40, y: 0, width: 40, height: 26 });
        assert_eq!(results.x + results.width, preview.x);
        assert_eq!(preview.x + preview.width, screen.full.unwrap().width);
        assert_eq!(results.height, preview.height);
    }

    #[test]
    fn a_narrow_pane_keeps_the_list_whole() {
        for cols in 0..MIN_HALF * 2 {
            let screen = Screen::new(30, cols);
            assert!(screen.preview.is_none(), "{cols} columns");
            assert_eq!(screen.results, screen.full, "{cols} columns");
        }
        let screen = Screen::new(30, MIN_HALF * 2 + 1);
        assert_eq!(screen.results.unwrap().width, MIN_HALF);
        assert_eq!(screen.preview.unwrap().width, MIN_HALF + 1);
    }

    #[test]
    fn seven_rows_is_the_last_pane_with_a_results_box() {
        let screen = Screen::new(7, 40);
        assert_eq!(screen.results.map(|r| r.inner_height()), Some(1));
        assert_eq!(screen.help_y, Some(6));

        assert!(Screen::new(6, 80).results.is_none());
        assert!(Screen::new(6, 80).preview.is_none());
        assert_eq!(Screen::new(6, 40).help_y, Some(5));
    }

    #[test]
    fn the_input_box_is_the_last_thing_to_go() {
        let screen = Screen::new(3, 40);
        assert!(screen.results.is_none());
        assert_eq!(screen.help_y, None);
        assert!(screen.bordered);
        assert_eq!(screen.input, Rect { x: 0, y: 0, width: 40, height: 3 });

        let screen = Screen::new(2, 40);
        assert!(!screen.bordered);
        assert_eq!(screen.input.height, 1);
        assert_eq!(screen.input.y, 1);
        assert_eq!(Screen::new(1, 40).input.y, 0);
    }

    #[test]
    fn nothing_is_ever_placed_off_the_pane() {
        for rows in 1..40 {
            let screen = Screen::new(rows, 40);
            assert!(screen.input.bottom_y() < rows, "{rows} rows");
            if let Some(y) = screen.help_y {
                assert!(y < rows);
                assert!(screen.input.bottom_y() < y);
            }
            if let Some(results) = screen.results {
                assert!(results.bottom_y() < screen.input.y);
                assert!(results.inner_height() >= 1);
            }
            if let Some(preview) = screen.preview {
                assert!(preview.bottom_y() < screen.input.y);
                assert!(preview.inner_height() >= 1);
            }
        }
    }

    #[test]
    fn a_block_is_anchored_to_the_bottom() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 10 };
        assert_eq!(rect.inner_height(), 8);
        assert_eq!(anchor(&rect, 1, 3), Block { y: 5, notes: 1, rows: 3 });
        let block = anchor(&rect, 1, 3);
        assert_eq!(block.y + block.notes + block.rows - 1, rect.bottom_y() - 1);
        assert_eq!(anchor(&rect, 0, 8), Block { y: 1, notes: 0, rows: 8 });
    }

    #[test]
    fn an_overfull_block_drops_rows_before_notes() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 5 };
        assert_eq!(rect.inner_height(), 3);
        assert_eq!(anchor(&rect, 2, 10), Block { y: 1, notes: 2, rows: 1 });
        assert_eq!(anchor(&rect, 5, 10), Block { y: 1, notes: 3, rows: 0 });
        for notes in 0..6 {
            for rows in 0..12 {
                let block = anchor(&rect, notes, rows);
                assert!(block.notes + block.rows <= rect.inner_height());
                assert_eq!(block.y + block.notes + block.rows, rect.inner_y() + rect.inner_height());
            }
        }
    }

    #[test]
    fn a_border_carries_its_labels() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 5 };
        let border = rect.top("Results", "3/47");
        assert_eq!(border.line, "╭─ Results ─────────── 3/47 ─╮");
        assert_eq!(border.line.width(), 30);
        let chars: Vec<char> = border.line.chars().collect();
        let at = |range: Range<usize>| chars[range].iter().collect::<String>();
        assert_eq!(at(border.title.clone().unwrap()), "Results");
        assert_eq!(at(border.right.clone().unwrap()), "3/47");
        assert_eq!(border.rule_indices().len(), 30 - 7 - 4);
    }

    #[test]
    fn a_narrow_border_drops_its_labels() {
        for width in 0..32 {
            let rect = Rect { x: 0, y: 0, width, height: 5 };
            let border = rect.top("Results", "3/47");
            assert_eq!(border.line.width(), width, "width {width}");
            assert_eq!(rect.bottom().width(), width, "width {width}");
        }
        assert_eq!(Rect { x: 0, y: 0, width: 8, height: 5 }.top("Results", "3/47").line, "╭──────╮");
    }

    #[test]
    fn a_line_is_padded_to_the_interior_and_carries_no_border() {
        let mut line = Line::new();
        line.push("luneta", 1);
        assert_eq!(line.columns(), 6);
        assert_eq!(line.finish(12).content(), " luneta       ");
    }

    #[test]
    fn a_line_pads_in_columns_not_characters() {
        let mut line = Line::new();
        line.push("日本語版", 1);
        assert_eq!(line.columns(), 8);
        assert_eq!(line.finish(12).content().width(), 14);
    }

    #[test]
    fn an_overlong_line_is_cut_back_to_the_box() {
        let mut line = Line::new();
        line.push("a-very-long-session-name", 1);
        line.gap(2);
        line.push("[R]", 0);
        let text = line.finish(10);
        assert_eq!(text.content(), " a-very-lo… ");
        assert_eq!(text.content().width(), 12);
    }

    #[test]
    fn truncate_leaves_text_that_fits() {
        assert_eq!(truncate("despesas", 8), "despesas");
        assert_eq!(truncate("despesas", 20), "despesas");
        assert_eq!(truncate("", 0), "");
    }

    #[test]
    fn truncate_pays_for_its_own_marker() {
        assert_eq!(truncate("despesas", 5), "desp…");
        assert_eq!(truncate("despesas", 1), "…");
        for max in 0..12 {
            assert!(truncate("despesas", max).width() <= max.max(1));
        }
        assert_eq!("日本語版".width(), 8);
        assert_eq!(truncate("日本語版", 5), "日本…");
        assert!(truncate("日本語版", 5).width() <= 5);
    }

    #[test]
    fn truncate_left_keeps_the_tail_and_says_what_it_dropped() {
        assert_eq!(truncate_left("luneta", 12), ("luneta".to_string(), 0));
        assert_eq!(
            truncate_left("/home/you/projects/luneta", 12),
            ("…ects/luneta".to_string(), 14)
        );
        assert!(truncate_left("/home/you/projects/luneta", 12).0.width() <= 12);

        let (out, dropped) = truncate_left("abcdefghij", 5);
        assert_eq!((out.as_str(), dropped), ("…ghij", 6));
        assert_eq!(out.chars().count(), 10 - dropped + 1);
    }
}
