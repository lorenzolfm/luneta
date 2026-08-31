//! Where things go, and how wide they can be.
//!
//! This module is arithmetic over rows and columns, with no host call. The geometry was once
//! inline in five render functions, where only an installed plugin could show whether it was
//! correct. Here it can be tested.
//!
//! Two units are in use. Columns are what the terminal draws, and they decide what fits.
//! Characters are what `Text::color_range` indexes. The two agree until a session is named
//! `日本語`, where one glyph takes two columns. [`Line`] counts columns as it builds a line and
//! returns character ranges, so that no caller has to hold both units.

use std::ops::Range;

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::Text;

/// Always rounded.
///
/// These once had to agree with the zellij frame outside them, which follows the
/// `rounded_corners` setting of the user. Only a `ModeUpdate` subscription can read that
/// setting, and this plugin does not subscribe to it. The pane is now `borderless` (see
/// `resize_self` in `main.rs`), so there is no outer frame and these are the corners of the
/// pane.
const TOP_LEFT: char = '╭';
const TOP_RIGHT: char = '╮';
const BOTTOM_LEFT: char = '╰';
const BOTTOM_RIGHT: char = '╯';
const HORIZONTAL: char = '─';
pub const VERTICAL: char = '│';

/// The narrowest title worth printing. Below this the title goes, because `R…` costs the same
/// four columns as a full title and names nothing. A plain rule is still a border.
const MIN_TITLE: usize = 4;

/// The columns a box uses on each side: the border, and one blank column after it.
pub const PAD: usize = 2;

/// A box on the pane, borders included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    /// The first row inside the box.
    pub fn inner_y(&self) -> usize {
        self.y + 1
    }

    /// The last row of the box, which holds the bottom border.
    pub fn bottom_y(&self) -> usize {
        self.y + self.height.saturating_sub(1)
    }

    /// How many rows fit between the borders.
    pub fn inner_height(&self) -> usize {
        self.height.saturating_sub(2)
    }

    /// How many columns of content fit between the borders and their padding.
    pub fn inner_width(&self) -> usize {
        self.width.saturating_sub(PAD * 2)
    }

    /// `╭─ Title ──────────────── right ─╮`, and the position of the two labels.
    ///
    /// Both labels are optional. A box that is too narrow for a label drops it and draws a
    /// plain rule. A title that is only long is truncated, because its first characters name
    /// the screen.
    ///
    /// The ranges are returned because the rule and the labels take different colours. The
    /// caller would otherwise have to find the title in a string it did not build.
    pub fn top(&self, title: &str, right: &str) -> Border {
        let Some(mut rule) = self.width.checked_sub(2) else {
            return Border::rule(HORIZONTAL.to_string().repeat(self.width));
        };
        let mut line = String::from(TOP_LEFT);
        let mut tail = String::new();
        let mut title_at = None;

        // The right label is measured first. It is a short count, and a long title must not
        // remove it.
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
        // `tail` starts with a space, so the label starts one character later.
        let right_start = line.chars().count() + 1;
        line.push_str(&tail);
        line.push(TOP_RIGHT);
        Border {
            line,
            title: title_at,
            right: has_right.then(|| right_start..right_start + right.chars().count()),
        }
    }

    /// `╰──────────────────────────────╯`
    pub fn bottom(&self) -> String {
        let Some(rule) = self.width.checked_sub(2) else {
            return HORIZONTAL.to_string().repeat(self.width);
        };
        let mut line = String::from(BOTTOM_LEFT);
        line.extend(std::iter::repeat_n(HORIZONTAL, rule));
        line.push(BOTTOM_RIGHT);
        line
    }

    /// An empty interior row: `│                    │`.
    pub fn blank(&self) -> String {
        Line::new().finish(self.inner_width()).content().to_string()
    }

}

/// The top border of a box: the line, and the position of its two labels, in characters.
pub struct Border {
    pub line: String,
    pub title: Option<Range<usize>>,
    pub right: Option<Range<usize>>,
}

impl Border {
    fn rule(line: String) -> Self {
        Self { line, title: None, right: None }
    }

    /// Every character that is rule and not label. These are dimmed.
    ///
    /// The list is built by exclusion. `Text` keeps one index list for each emphasis level and
    /// allows a character in more than one list, so overlapping ranges leave the host to
    /// resolve a conflict that this code never states.
    pub fn rule_indices(&self) -> Vec<usize> {
        let labelled = |i: &usize| {
            self.title.as_ref().is_some_and(|r| r.contains(i))
                || self.right.as_ref().is_some_and(|r| r.contains(i))
        };
        (0..self.line.chars().count()).filter(|i| !labelled(i)).collect()
    }
}

/// One row inside a box, built from left to right.
///
/// The row records its own styling as it grows. Emphasis applies to character indexes, and
/// budgets apply to display columns. A caller that wrote a name, a gap and a tag would
/// otherwise have to compute which characters the tag holds.
///
/// All that is not styled (the padding and the gaps between columns) becomes dim by exclusion.
/// A dim line with content drawn over it would put characters in two emphasis levels, and
/// `Text` states no precedence between its levels.
pub struct Line {
    text: String,
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
    /// Open an empty row. Offsets start at the first column of content. [`Line::finish`] adds
    /// the padding and moves every offset past it. Content and frame are built separately, so
    /// that `finish` can cut a long row without cutting a border.
    pub fn new() -> Self {
        Self { text: String::new(), columns: 0, styles: Vec::new() }
    }

    /// How many columns of content have been written.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Append text at emphasis `level`.
    pub fn push(&mut self, text: &str, level: usize) {
        let range = self.raw(text);
        self.styles.push(Style::Level(level, range));
    }

    /// Append text at emphasis `level`. `hits` are character offsets into `text` and take the
    /// `accent` level. This paints the characters a fuzzy term matched.
    pub fn push_hits(&mut self, text: &str, level: usize, accent: usize, hits: &[usize]) {
        let range = self.raw(text);
        let start = range.start;
        self.styles.push(Style::Level(level, range));
        self.styles.push(Style::Hits(accent, hits.iter().map(|i| start + i).collect()));
    }

    /// Append text in the error colour.
    pub fn push_error(&mut self, text: &str) {
        let range = self.raw(text);
        self.styles.push(Style::Error(range));
    }

    /// Append `n` blank columns.
    pub fn gap(&mut self, n: usize) {
        self.text.extend(std::iter::repeat_n(' ', n));
        self.columns += n;
    }

    /// Pad to `columns` of content. This never truncates: a caller that passed its budget has
    /// a defect, and this method must not hide it.
    pub fn pad_to(&mut self, columns: usize) {
        self.gap(columns.saturating_sub(self.columns));
    }

    fn raw(&mut self, text: &str) -> Range<usize> {
        let start = self.text.chars().count();
        self.text.push_str(text);
        self.columns += text.width();
        start..self.text.chars().count()
    }

    /// Pad the row to the interior width of its box and make a styled `Text` of exactly
    /// `inner_width + 2` columns.
    ///
    /// The borders are not part of the row. A selected row is one `Text::selected()`, and the
    /// host paints the selected background over the whole `Text`. A row that held its own `│`
    /// characters thus lost both sides of the box under the highlight.
    /// [`crate::render::draw_row`] draws the frame beside the row instead.
    ///
    /// The truncation here is the last check, not the main fit. Each screen drops columns
    /// before a row overflows, but each screen also keeps a minimum for the name column, and on
    /// a very narrow pane that minimum is more than the space left. The `Table` this replaced
    /// hid the problem, because the host removed any column that did not fit. A row one column
    /// too long now writes over the right border, so the width is enforced here.
    pub fn finish(self, inner_width: usize) -> Text {
        let Line { mut text, mut columns, styles } = self;
        if columns > inner_width {
            text = truncate(&text, inner_width);
            columns = text.width();
        }
        let visible = text.chars().count();

        let mut line = String::from(" ");
        line.push_str(&text);
        line.extend(std::iter::repeat_n(' ', inner_width - columns));
        line.push(' ');

        // One column of padding is before the content, so every offset moves by one. What the
        // truncation removed is dropped, because a colour on a character that is gone paints a
        // different character.
        let shift = |i: usize| i + 1;
        let clamp = |range: Range<usize>| {
            let start = range.start.min(visible);
            let end = range.end.min(visible);
            (start < end).then(|| shift(start)..shift(end))
        };

        let styled: Vec<usize> = styles
            .iter()
            .flat_map(|style| match style {
                Style::Level(_, range) | Style::Error(range) => {
                    clamp(range.clone()).map(|r| r.collect::<Vec<_>>()).unwrap_or_default()
                },
                Style::Hits(_, indices) => {
                    indices.iter().filter(|i| **i < visible).map(|i| shift(*i)).collect()
                },
            })
            .collect();
        let frame: Vec<usize> =
            (0..line.chars().count()).filter(|i| !styled.contains(i)).collect();

        // The host draws every character of a `Text` bold unless it is told otherwise
        // (`ui/components/text.rs`: the base style is `.bold(On)`, and only index level 5
        // removes it). A screen of bold text has no bold left for a title. The content is
        // therefore made not bold here, and the titles in [`crate::render::border_text`] stay
        // bold.
        //
        // Only the styled characters, not the frame: the host tests unbold before dim and stops
        // at the first match, so an unbold on a dim character would remove the dim.
        let mut text = Text::new(&line).dim_indices(frame).unbold_indices(styled);
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

/// How the pane is divided.
///
/// The order of removal follows what you cannot do without. The input box stays after the
/// results box goes, because a picker must show what you type. The borders stay after the help
/// line goes. A pane of one row holds the input line alone.
pub struct Screen {
    /// The results box. `None` when the pane cannot hold one with a row in it. It takes the
    /// left half when [`Screen::preview`] is beside it.
    pub results: Option<Rect>,
    /// The preview box, on the right. `None` on a pane too narrow to divide, and on the screens
    /// that preview nothing.
    pub preview: Option<Rect>,
    /// The undivided results area, for the screens that preview nothing. The confirm and rename
    /// screens each ask one question and use the full width.
    pub full: Option<Rect>,
    /// The input box. It is always present. When [`Screen::bordered`] is false it is one row
    /// with no border.
    pub input: Rect,
    pub bordered: bool,
    /// The help line, on the last row of the pane. `None` when no row is free for it.
    pub help_y: Option<usize>,
}

/// The input box: top border, the line you type into, bottom border.
const INPUT_HEIGHT: usize = 3;

/// The shortest results box worth drawing: two borders and a single row of list.
const MIN_RESULTS: usize = 3;

/// The narrowest half worth making.
///
/// This applies to both halves, so a division needs twice this width. Below it the preview goes
/// and the list takes the full width, because a preview of three columns says nothing.
///
/// Twenty-six columns leave twenty-two columns of content on each side: a gutter, a name and an
/// age on the left, and a tab name and a pane title on the right.
const MIN_HALF: usize = 26;

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        let help_y = (rows > INPUT_HEIGHT).then(|| rows - 1);
        let bordered = rows >= INPUT_HEIGHT;
        let height = if bordered { INPUT_HEIGHT } else { 1 };
        let input_y = rows.saturating_sub(usize::from(help_y.is_some()) + height);
        let input = Rect { x: 0, y: input_y, width: cols, height };
        // What the input box and the help line leave, if it can hold a row.
        let full = (input_y >= MIN_RESULTS)
            .then_some(Rect { x: 0, y: 0, width: cols, height: input_y });
        // The two boxes touch. Their borders are the separation, and a gap would take a third
        // column of width. An odd column goes to the preview.
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

/// Where a block of notes and rows sits inside a box.
///
/// The block is anchored to the bottom, so that the list is next to the prompt and the notes
/// are above the list. Two of the three notes explain an absence from the list, such as `you
/// are in "X" — not listed`, so they must be next to it.
#[derive(Debug, PartialEq, Eq)]
pub struct Block {
    /// The row the first note (or, with no notes, the first result) is drawn on.
    pub y: usize,
    /// How many note lines fit.
    pub notes: usize,
    /// How many result rows fit.
    pub rows: usize,
}

/// The notes get their rows first. They are two lines at most, and they are the only text that
/// can explain why the list is short.
pub fn anchor(rect: &Rect, notes: usize, rows: usize) -> Block {
    let height = rect.inner_height();
    let notes = notes.min(height);
    let rows = rows.min(height - notes);
    Block { y: rect.inner_y() + (height - notes - rows), notes, rows }
}

/// Truncate to `max` columns and mark the cut with `…`.
///
/// The marker comes out of the budget, so the result never exceeds `max`.
pub fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = ch.to_string().width();
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

/// Truncate from the left, keep the tail, and report how many characters were removed.
///
/// The last components identify a path. The first components are `/home/you/` on every row. The
/// count is returned because the caller holds match positions in the original string and must
/// move the positions that remain.
pub fn truncate_left(text: &str, max: usize) -> (String, usize) {
    if text.width() <= max {
        return (text.to_string(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut width = 0;
    let mut kept = 0;
    for ch in chars.iter().rev() {
        let w = ch.to_string().width();
        // One column is kept for the `…` that replaces what is removed.
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

    /// The usual pane: results box on top, input box above the help row.
    #[test]
    fn a_tall_pane_gets_both_boxes() {
        let screen = Screen::new(30, 80);
        let full = screen.full.expect("a 30-row pane has room for a results box");
        assert_eq!(full, Rect { x: 0, y: 0, width: 80, height: 26 });
        assert_eq!(screen.input, Rect { x: 0, y: 26, width: 80, height: 3 });
        assert_eq!(screen.help_y, Some(29));
        assert!(screen.bordered);
        // Nothing overlaps, and no row is unused.
        assert_eq!(full.bottom_y() + 1, screen.input.y);
        assert_eq!(screen.input.bottom_y() + 1, screen.help_y.unwrap());
    }

    /// A wide pane is divided in the middle: list on the left, preview on the right. Together
    /// they cover the area of the undivided box.
    #[test]
    fn a_wide_pane_puts_a_preview_beside_the_list() {
        let screen = Screen::new(30, 80);
        let results = screen.results.expect("80 columns is wide enough to split");
        let preview = screen.preview.expect("80 columns is wide enough to split");
        assert_eq!(results, Rect { x: 0, y: 0, width: 40, height: 26 });
        assert_eq!(preview, Rect { x: 40, y: 0, width: 40, height: 26 });
        // They touch, they cover the full width, and they have the same height.
        assert_eq!(results.x + results.width, preview.x);
        assert_eq!(preview.x + preview.width, screen.full.unwrap().width);
        assert_eq!(results.height, preview.height);
    }

    /// Below twice [`MIN_HALF`] the preview goes and the list takes the full width. An odd
    /// column goes to the preview.
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

    /// Seven rows is the shortest pane that holds everything: two borders and one row of list,
    /// then the input box, then the help line.
    #[test]
    fn seven_rows_is_the_last_pane_with_a_results_box() {
        let screen = Screen::new(7, 40);
        assert_eq!(screen.results.map(|r| r.inner_height()), Some(1));
        assert_eq!(screen.help_y, Some(6));

        // At six rows the results box is two borders with nothing between them, so it goes,
        // and the preview goes with it.
        assert!(Screen::new(6, 80).results.is_none());
        assert!(Screen::new(6, 80).preview.is_none());
        assert_eq!(Screen::new(6, 40).help_y, Some(5));
    }

    /// The order of removal: the results box goes first, then the help line, then the borders.
    /// The row you type into is the last to go.
    #[test]
    fn the_input_box_is_the_last_thing_to_go() {
        // No room for the help line, but still a bordered input box.
        let screen = Screen::new(3, 40);
        assert!(screen.results.is_none());
        assert_eq!(screen.help_y, None);
        assert!(screen.bordered);
        assert_eq!(screen.input, Rect { x: 0, y: 0, width: 40, height: 3 });

        // No room for borders: one row, and it is the prompt.
        let screen = Screen::new(2, 40);
        assert!(!screen.bordered);
        assert_eq!(screen.input.height, 1);
        assert_eq!(screen.input.y, 1);
        assert_eq!(Screen::new(1, 40).input.y, 0);
    }

    /// At every size, the boxes stay inside the pane and in order.
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

    /// The block sits at the bottom of its box, with the notes above the rows.
    #[test]
    fn a_block_is_anchored_to_the_bottom() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 10 };
        assert_eq!(rect.inner_height(), 8);
        assert_eq!(anchor(&rect, 1, 3), Block { y: 5, notes: 1, rows: 3 });
        // The last row of the block is the last row inside the box.
        let block = anchor(&rect, 1, 3);
        assert_eq!(block.y + block.notes + block.rows - 1, rect.bottom_y() - 1);
        // A block that fills the box starts on its first row.
        assert_eq!(anchor(&rect, 0, 8), Block { y: 1, notes: 0, rows: 8 });
    }

    /// The notes get their rows first. They are the only text that can explain why the list is
    /// short.
    #[test]
    fn an_overfull_block_drops_rows_before_notes() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 5 };
        assert_eq!(rect.inner_height(), 3);
        assert_eq!(anchor(&rect, 2, 10), Block { y: 1, notes: 2, rows: 1 });
        assert_eq!(anchor(&rect, 5, 10), Block { y: 1, notes: 3, rows: 0 });
        // Never taller than the box, at any input.
        for notes in 0..6 {
            for rows in 0..12 {
                let block = anchor(&rect, notes, rows);
                assert!(block.notes + block.rows <= rect.inner_height());
                assert_eq!(block.y + block.notes + block.rows, rect.inner_y() + rect.inner_height());
            }
        }
    }

    /// `╭─ Title ──────── right ─╮`, with both labels placed and their positions returned.
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
        // All else is rule, and is dimmed.
        assert_eq!(border.rule_indices().len(), 30 - 7 - 4);
    }

    /// A box that is too narrow for a label drops it. A plain rule is still a border.
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

    /// A finished row is its content, padded to the interior of the box. It carries no border,
    /// so that the highlight of a selected row cannot cover one.
    #[test]
    fn a_line_is_padded_to_the_interior_and_carries_no_border() {
        let mut line = Line::new();
        line.push("luneta", 1);
        assert_eq!(line.columns(), 6);
        assert_eq!(line.finish(12).content(), " luneta       ");
    }

    /// Columns, not characters. Four CJK characters take eight columns, and padding that
    /// counts characters puts the right border two cells too far left.
    #[test]
    fn a_line_pads_in_columns_not_characters() {
        let mut line = Line::new();
        line.push("日本語版", 1);
        assert_eq!(line.columns(), 8);
        assert_eq!(line.finish(12).content().width(), 14);
    }

    /// A row that is wider than its box is cut, and does not write over the border. See
    /// [`Line::finish`].
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

    /// Text that fits is returned unchanged, with no marker.
    #[test]
    fn truncate_leaves_text_that_fits() {
        assert_eq!(truncate("despesas", 8), "despesas");
        assert_eq!(truncate("despesas", 20), "despesas");
        assert_eq!(truncate("", 0), "");
    }

    /// The `…` comes out of the budget, so the result never exceeds `max`.
    #[test]
    fn truncate_pays_for_its_own_marker() {
        assert_eq!(truncate("despesas", 5), "desp…");
        assert_eq!(truncate("despesas", 1), "…");
        for max in 0..12 {
            assert!(truncate("despesas", max).width() <= max.max(1));
        }
        // Four characters take eight columns.
        assert_eq!("日本語版".width(), 8);
        assert_eq!(truncate("日本語版", 5), "日本…");
        assert!(truncate("日本語版", 5).width() <= 5);
    }

    /// The last components identify a path, so the tail is kept. The caller moves its match
    /// indexes by the count of removed characters.
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
