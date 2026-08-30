//! Where things go, and how wide they are allowed to be.
//!
//! Everything in this module is pure arithmetic over rows and columns, with no host call in
//! sight. That is deliberate and it is the point: the picker's geometry used to live inline in
//! five render functions, where the only way to check it was to install the plugin and look at
//! a floating pane. Here it can be tested.
//!
//! Two units are in play and they are not the same. **Columns** are what the terminal draws —
//! a CJK name is one column per half of each glyph — and they decide what fits. **Characters**
//! are what `Text::color_range` indexes. They coincide until someone names a session `日本語`,
//! at which point conflating them paints the wrong half of the wrong glyph. [`Row`] is the
//! answer: it counts columns as you build a line and hands back character ranges as you go, so
//! no caller has to hold both in its head.

use std::ops::Range;

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::Text;

/// Rounded, unconditionally.
///
/// Zellij's own frame — drawn immediately outside ours, since a plugin cannot turn off its own
/// frame without `set_pane_frame_style` restyling every pane in the session — follows the
/// user's `rounded_corners` setting, which is reachable only through a `ModeUpdate`
/// subscription this plugin deliberately does not have (see [`crate::render`]'s module doc).
/// So on `rounded_corners false` the two borders disagree by one cell. That is the price of not
/// carrying a `Style` through every render function, and it is a cell.
const TOP_LEFT: char = '╭';
const TOP_RIGHT: char = '╮';
const BOTTOM_LEFT: char = '╰';
const BOTTOM_RIGHT: char = '╯';
const HORIZONTAL: char = '─';
const VERTICAL: char = '│';

/// The narrowest a box title is worth printing. Below this it is dropped: `R…` costs the same
/// four columns of chrome as a real title and names nothing, and a plain rule still reads as a
/// border.
const MIN_TITLE: usize = 4;

/// The columns a box spends on itself per side: the border, then one blank column so that text
/// does not sit flush against it.
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

    /// The box's last row — the one the bottom border is drawn on.
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

    /// `╭─ Title ──────────────── right ─╮`, and where the two labels landed.
    ///
    /// Both labels are optional and both are dropped rather than cramped: a box too narrow to
    /// hold its own title gets a plain rule, which still reads as a border. A title that is
    /// merely long is truncated, because the screen name's first characters are the half that
    /// tells you which screen you are on.
    ///
    /// The ranges come back because the rule and the labels are coloured differently, and the
    /// alternative — the caller finding the title in the string it did not build — is the
    /// offset arithmetic this module exists to stop doing by hand.
    pub fn top(&self, title: &str, right: &str) -> Border {
        let Some(mut rule) = self.width.checked_sub(2) else {
            return Border::rule(HORIZONTAL.to_string().repeat(self.width));
        };
        let mut line = String::from(TOP_LEFT);
        let mut tail = String::new();
        let mut title_at = None;

        // The right-hand label is measured first: it is a count, so it is short, and losing it
        // to a long title would make it flicker in and out as the title changed.
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
        // `tail` opens with a space, so the label starts one character in.
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

    /// An empty interior row — `│                    │`.
    pub fn blank(&self) -> String {
        Line::new().finish(self.inner_width()).content().to_string()
    }

}

/// A box's top border: the line, and where its two labels sit in it, in characters.
pub struct Border {
    pub line: String,
    pub title: Option<Range<usize>>,
    pub right: Option<Range<usize>>,
}

impl Border {
    fn rule(line: String) -> Self {
        Self { line, title: None, right: None }
    }

    /// Every character that is rule rather than label — what gets dimmed.
    ///
    /// Built by exclusion rather than by colouring the whole line and painting the labels back
    /// over it: `Text` keeps one index list per emphasis level and lets a character sit in
    /// several at once, so overlapping ranges leave the host to break a tie we never stated.
    pub fn rule_indices(&self) -> Vec<usize> {
        let labelled = |i: &usize| {
            self.title.as_ref().is_some_and(|r| r.contains(i))
                || self.right.as_ref().is_some_and(|r| r.contains(i))
        };
        (0..self.line.chars().count()).filter(|i| !labelled(i)).collect()
    }
}

/// One row inside a box, built left to right.
///
/// The row keeps its own styling as it goes, and that is the point. Emphasis is applied by
/// **character** index and column budgets are decided in **display columns**; a caller that
/// wrote a name, a gap and a tag and then had to say which characters the tag occupied would be
/// re-deriving offsets from strings it wrote three statements ago. Here the offsets never
/// leave the type that knows them.
///
/// Everything not explicitly styled — the borders, the padding, the gaps between columns —
/// comes out dim, by exclusion. Colouring the whole line dim and painting content back over it
/// would leave characters in two emphasis levels at once, and `Text` keeps one index list per
/// level with no stated precedence between them, so the host would be breaking a tie we never
/// meant to hand it.
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
    /// Opens an empty row. Offsets run from the first column of *content*; the border and its
    /// padding are put on in [`Line::finish`], which is also where every recorded offset is
    /// shifted past them. Building content and frame separately is what lets `finish` cut an
    /// overlong row back to the box without slicing through a border.
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

    /// Append text at emphasis `level`, with `hits` — character offsets *into `text`* — raised
    /// to `accent`. This is how a fuzzy match paints the characters it matched.
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

    /// Pad out to `columns` of content. Never truncates — a caller that overran its budget has
    /// a bug that hiding here would only make harder to find.
    pub fn pad_to(&mut self, columns: usize) {
        self.gap(columns.saturating_sub(self.columns));
    }

    fn raw(&mut self, text: &str) -> Range<usize> {
        let start = self.text.chars().count();
        self.text.push_str(text);
        self.columns += text.width();
        start..self.text.chars().count()
    }

    /// Wrap the row in its border and turn it into a styled `Text`, exactly `inner_width + 4`
    /// columns wide.
    ///
    /// ⚠️ The truncation here is a backstop, not the primary fit. Each screen has a ladder that
    /// decides what to drop before anything overflows, but those ladders floor the name column
    /// at a few columns so that a name is never reduced to nothing — which on a pane narrow
    /// enough means the floor exceeds what is left. The `Table` this replaced hid that: the
    /// host silently dropped any column that did not fit. Nothing hides it now, and a row one
    /// column too long puts a character where the right border goes, so the width is enforced
    /// at the one place every row passes through.
    pub fn finish(self, inner_width: usize) -> Text {
        let Line { mut text, mut columns, styles } = self;
        if columns > inner_width {
            text = truncate(&text, inner_width);
            columns = text.width();
        }
        let visible = text.chars().count();

        let mut line = String::from(VERTICAL);
        line.push(' ');
        line.push_str(&text);
        line.extend(std::iter::repeat_n(' ', inner_width - columns));
        line.push(' ');
        line.push(VERTICAL);

        // The border and its padding are two single-column characters, so every content offset
        // moves along by two — and anything the truncation above took is dropped rather than
        // pointed at, because colouring a character that is no longer there paints another one.
        let shift = |i: usize| i + 2;
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

        let mut text = Text::new(&line).dim_indices(frame);
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

/// How the pane is cut up.
///
/// Three tiers, and the ladder is about what you cannot do without. The input box outlives the
/// results box because a picker that cannot show you what you are typing is not a picker; the
/// borders outlive the help line for the same reason in miniature. Below all of it the pane is
/// a single row and gets the one line that matters.
pub struct Screen {
    /// The results box — `None` when the pane cannot hold one with anything in it.
    pub results: Option<Rect>,
    /// The input box. Always present; when [`Screen::bordered`] is false it is the bare
    /// prompt row, one row tall, with no border to draw.
    pub input: Rect,
    pub bordered: bool,
    /// The help line, on the pane's last row — `None` when there is no row to spare for it.
    pub help_y: Option<usize>,
}

/// The input box: top border, the line you type into, bottom border.
const INPUT_HEIGHT: usize = 3;

/// The shortest results box worth drawing: two borders and a single row of list.
const MIN_RESULTS: usize = 3;

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        let help_y = (rows > INPUT_HEIGHT).then(|| rows - 1);
        let bordered = rows >= INPUT_HEIGHT;
        let height = if bordered { INPUT_HEIGHT } else { 1 };
        let input_y = rows.saturating_sub(usize::from(help_y.is_some()) + height);
        let input = Rect { x: 0, y: input_y, width: cols, height };
        // Whatever the input box and the help line left behind, if it is enough to hold a row.
        let results = (input_y >= MIN_RESULTS)
            .then_some(Rect { x: 0, y: 0, width: cols, height: input_y });
        Self { results, input, bordered, help_y }
    }
}

/// Where a `[notes][rows]` block lands inside a box.
///
/// Bottom-anchored, as one block: the list hugs the prompt instead of stranding itself at the
/// top of a mostly empty box, and the notes ride on top of the list rather than being pinned to
/// the box. Two of the three notes exist to explain *an absence from the list* — "you are in
/// X — not listed" — so flush against the list is where they read correctly, and between the
/// list and the caret is the one place they must not go.
#[derive(Debug, PartialEq, Eq)]
pub struct Block {
    /// The row the first note (or, with no notes, the first result) is drawn on.
    pub y: usize,
    /// How many note lines fit.
    pub notes: usize,
    /// How many result rows fit.
    pub rows: usize,
}

/// Notes are served first. They are at most two lines and they are the only thing on screen
/// that can explain why the list is short — dropping them to fit one more row of the list
/// answers the wrong question.
pub fn anchor(rect: &Rect, notes: usize, rows: usize) -> Block {
    let height = rect.inner_height();
    let notes = notes.min(height);
    let rows = rows.min(height - notes);
    Block { y: rect.inner_y() + (height - notes - rows), notes, rows }
}

/// Truncate to `max` **columns**, marking the cut with `…`.
///
/// The marker is paid for out of the budget, so the result never exceeds `max`.
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

/// Truncate from the **left**, keeping the tail, and report how many characters went.
///
/// A path is identified by its last components; its first are `/home/you/` on every row of the
/// list. The count comes back because the caller is holding match positions into the original
/// string and has to shift the ones that survived.
pub fn truncate_left(text: &str, max: usize) -> (String, usize) {
    if text.width() <= max {
        return (text.to_string(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut width = 0;
    let mut kept = 0;
    for ch in chars.iter().rev() {
        let w = ch.to_string().width();
        // One column is held back for the `…` that replaces everything dropped.
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

    /// The ordinary pane: results box on top, input box above the help row.
    #[test]
    fn a_tall_pane_gets_both_boxes() {
        let screen = Screen::new(30, 80);
        let results = screen.results.expect("a 30-row pane has room for a results box");
        assert_eq!(results, Rect { x: 0, y: 0, width: 80, height: 26 });
        assert_eq!(screen.input, Rect { x: 0, y: 26, width: 80, height: 3 });
        assert_eq!(screen.help_y, Some(29));
        assert!(screen.bordered);
        // Nothing overlaps, and nothing is left over.
        assert_eq!(results.bottom_y() + 1, screen.input.y);
        assert_eq!(screen.input.bottom_y() + 1, screen.help_y.unwrap());
    }

    /// Seven rows is the shortest pane that still holds everything: two borders and one row of
    /// list, then the input box, then the help line.
    #[test]
    fn seven_rows_is_the_last_pane_with_a_results_box() {
        let screen = Screen::new(7, 40);
        assert_eq!(screen.results.map(|r| r.inner_height()), Some(1));
        assert_eq!(screen.help_y, Some(6));

        // At six, the results box would be borders with nothing between them, so it goes.
        assert!(Screen::new(6, 40).results.is_none());
        assert_eq!(Screen::new(6, 40).help_y, Some(5));
    }

    /// The ladder is about what you cannot do without: the input box outlives the results box,
    /// its borders outlive the help line, and the row you type into outlives everything.
    #[test]
    fn the_input_box_is_the_last_thing_to_go() {
        // No room for the help line, but still a bordered input box.
        let screen = Screen::new(3, 40);
        assert!(screen.results.is_none());
        assert_eq!(screen.help_y, None);
        assert!(screen.bordered);
        assert_eq!(screen.input, Rect { x: 0, y: 0, width: 40, height: 3 });

        // No room for borders either: one row, and it is the prompt.
        let screen = Screen::new(2, 40);
        assert!(!screen.bordered);
        assert_eq!(screen.input.height, 1);
        assert_eq!(screen.input.y, 1);
        assert_eq!(Screen::new(1, 40).input.y, 0);
    }

    /// Whatever the pane, the chrome stays inside it and in order.
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
        }
    }

    /// The block sits on the floor of its box, notes above rows.
    #[test]
    fn a_block_is_anchored_to_the_bottom() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 10 };
        assert_eq!(rect.inner_height(), 8);
        assert_eq!(anchor(&rect, 1, 3), Block { y: 5, notes: 1, rows: 3 });
        // The last row of the block is always the last row inside the box.
        let block = anchor(&rect, 1, 3);
        assert_eq!(block.y + block.notes + block.rows - 1, rect.bottom_y() - 1);
        // A block that fills the box starts at its first row.
        assert_eq!(anchor(&rect, 0, 8), Block { y: 1, notes: 0, rows: 8 });
    }

    /// Notes are served first. They are at most two lines and they are the only thing that can
    /// explain why the list is short — dropping one to fit another row answers the wrong
    /// question.
    #[test]
    fn an_overfull_block_drops_rows_before_notes() {
        let rect = Rect { x: 0, y: 0, width: 20, height: 5 };
        assert_eq!(rect.inner_height(), 3);
        assert_eq!(anchor(&rect, 2, 10), Block { y: 1, notes: 2, rows: 1 });
        assert_eq!(anchor(&rect, 5, 10), Block { y: 1, notes: 3, rows: 0 });
        // Never taller than the box, whatever it is handed.
        for notes in 0..6 {
            for rows in 0..12 {
                let block = anchor(&rect, notes, rows);
                assert!(block.notes + block.rows <= rect.inner_height());
                assert_eq!(block.y + block.notes + block.rows, rect.inner_y() + rect.inner_height());
            }
        }
    }

    /// `╭─ Title ──────── right ─╮`, with both labels placed and reported.
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
        // Everything else is rule, and gets dimmed.
        assert_eq!(border.rule_indices().len(), 30 - 7 - 4);
    }

    /// A box too narrow for a label drops it rather than cramping it — a plain rule still reads
    /// as a border, and a border that has eaten its own corner does not.
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

    /// A finished row is its content, framed, padded to exactly the box's width.
    #[test]
    fn a_line_is_framed_and_padded_to_its_box() {
        let mut line = Line::new();
        line.push("luneta", 1);
        assert_eq!(line.columns(), 6);
        assert_eq!(line.finish(12).content(), "│ luneta       │");
    }

    /// Columns, not characters. Four CJK characters are eight columns, and padding that counts
    /// characters puts the right border two cells short.
    #[test]
    fn a_line_pads_in_columns_not_characters() {
        let mut line = Line::new();
        line.push("日本語版", 1);
        assert_eq!(line.columns(), 8);
        assert_eq!(line.finish(12).content().width(), 16);
    }

    /// The backstop: a row built wider than its box is cut back rather than painting over the
    /// border. See [`Line::finish`].
    #[test]
    fn an_overlong_line_is_cut_back_to_the_box() {
        let mut line = Line::new();
        line.push("a-very-long-session-name", 1);
        line.gap(2);
        line.push("[R]", 0);
        let text = line.finish(10);
        assert_eq!(text.content(), "│ a-very-lo… │");
        assert_eq!(text.content().width(), 14);
    }

    /// Text that fits is returned untouched — no marker, no change.
    #[test]
    fn truncate_leaves_text_that_fits() {
        assert_eq!(truncate("despesas", 8), "despesas");
        assert_eq!(truncate("despesas", 20), "despesas");
        assert_eq!(truncate("", 0), "");
    }

    /// The `…` is paid for out of the budget, so the result never exceeds `max`.
    #[test]
    fn truncate_pays_for_its_own_marker() {
        assert_eq!(truncate("despesas", 5), "desp…");
        assert_eq!(truncate("despesas", 1), "…");
        for max in 0..12 {
            assert!(truncate("despesas", max).width() <= max.max(1));
        }
        // Four characters, eight columns.
        assert_eq!("日本語版".width(), 8);
        assert_eq!(truncate("日本語版", 5), "日本…");
        assert!(truncate("日本語版", 5).width() <= 5);
    }

    /// Keeping the tail is the whole point: a path is identified by its last components, and
    /// the dropped count is what the caller shifts its match indices by.
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
