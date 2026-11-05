//! The text the left button selects on the sidebar, dragged from the cell it
//! went down on to the one under the pointer: a sweep, as the rows `v` selects
//! are a selection. It goes line by line, as a terminal's selection does, and
//! stays in the sidebar's columns. It shows reversed over every frame, and it
//! reads the lines the sidebar drew last, so that what shows is what is copied.

use std::ops::Range;

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::spots::Spots;

/// What the sidebar drew last, for a sweep to read.
#[derive(Debug, Default)]
pub struct Page {
    /// Its lines, from the top of the sidebar.
    pub lines: Vec<Line<'static>>,
    /// Those of them that go on from the line above, as what a note wraps does.
    pub goes_on: Vec<usize>,
    /// What each `…` of a cut stands for, by its cell on luneta's terminal: what
    /// the cut took.
    pub cuts: Spots<String>,
}

/// Where the left button went down and where the pointer is, on luneta's
/// terminal, each kept to the cells a sweep has: the sidebar's, short of its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sweep {
    from: Position,
    to: Position,
}

impl Sweep {
    pub fn new(from: Position, to: Position, within: Rect) -> Sweep {
        Sweep { from: kept(from, within), to: kept(to, within) }
    }

    /// The pointer moves to `to`.
    pub fn reach(&mut self, to: Position, within: Rect) {
        self.to = kept(to, within);
    }

    /// Its two ends, the one that comes first on the screen first.
    fn ends(self) -> (Position, Position) {
        if (self.from.y, self.from.x) <= (self.to.y, self.to.x) {
            (self.from, self.to)
        } else {
            (self.to, self.from)
        }
    }

    /// The columns it takes of the line `y`, one of those between its ends: on
    /// to the end of the line, on all but the last of them, and from its start,
    /// on all but the first.
    fn columns(self, y: u16, within: Rect) -> Range<u16> {
        let (first, last) = self.ends();
        let start = if y == first.y { first.x } else { within.left() };
        let end = if y == last.y { last.x.saturating_add(1) } else { within.right() };
        start..end
    }

    /// Shows it reversed on `buffer`.
    pub fn draw(self, buffer: &mut Buffer, within: Rect) {
        let (first, last) = self.ends();
        for y in first.y..=last.y {
            let columns = self.columns(y, within);
            let cells = Rect::new(columns.start, y, columns.end.saturating_sub(columns.start), 1);
            let reversed = Style::new().add_modifier(Modifier::REVERSED);
            buffer.set_style(cells.intersection(within), reversed);
        }
    }

    /// What it holds of `page`, which is what you would copy: a line to a line,
    /// but for one that goes on from the line above, which it joins as it was
    /// before it wrapped. The blanks at the end of each line go, as do the blank
    /// lines around it all.
    pub fn text(self, page: &Page, within: Rect) -> String {
        let (first, last) = self.ends();
        let goes_on = |line: usize| page.goes_on.contains(&line);
        let mut text = String::new();
        for y in first.y..=last.y {
            let line = usize::from(y.saturating_sub(within.y));
            let mut columns = self.columns(y, within);
            // The space a wrapped line starts with, in from the edge, is no
            // part of what it says.
            if y > first.y && goes_on(line) {
                columns.start = columns.start.saturating_add(1);
            }
            let said = page.lines.get(line).map_or_else(String::new, |drawn| {
                read(drawn, within.x, columns, |x| page.cuts.at(Position::new(x, y)))
            });
            if y < last.y && goes_on(line + 1) {
                text.push_str(&said);
            } else {
                text.push_str(said.trim_end());
                text.push('\n');
            }
        }
        text.trim_start_matches('\n').trim_end().to_owned()
    }
}

/// `at`, or the cell of `within` nearest to it.
fn kept(at: Position, within: Rect) -> Position {
    let last = |start: u16, length: u16| start.saturating_add(length.saturating_sub(1));
    Position::new(
        at.x.clamp(within.x, last(within.x, within.width)),
        at.y.clamp(within.y, last(within.y, within.height)),
    )
}

/// What `line`, drawn from the column `left`, has on `columns`, where a `…` that
/// `cut` says the cut of comes out as what the cut took. A wide character counts
/// by its first cell, as how that one is drawn is how the character shows.
fn read<'a>(
    line: &Line<'_>,
    left: u16,
    columns: Range<u16>,
    cut: impl Fn(u16) -> Option<&'a String>,
) -> String {
    let mut text = String::new();
    let mut x = left;
    for grapheme in line.styled_graphemes(Style::new()) {
        let width = grapheme.symbol.cell_width();
        if width == 0 {
            continue;
        }
        if columns.contains(&x) {
            text.push_str(cut(x).map_or(grapheme.symbol, String::as_str));
        }
        x = x.saturating_add(width);
    }
    text
}

/// How many cells `text` takes as it is drawn, which is how `read` counts them.
pub fn cells(text: &str) -> usize {
    let span = Span::raw(text);
    span.styled_graphemes(Style::new()).map(|it| usize::from(it.symbol.cell_width())).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITHIN: Rect = Rect { x: 2, y: 1, width: 10, height: 6 };

    fn at(x: u16, y: u16) -> Position {
        Position::new(x, y)
    }

    fn page(lines: &[&'static str]) -> Page {
        Page { lines: lines.iter().map(|line| Line::raw(*line)).collect(), ..Page::default() }
    }

    fn swept(from: Position, to: Position, page: &Page) -> String {
        Sweep::new(from, to, WITHIN).text(page, WITHIN)
    }

    /// Along a line it takes the cells from one end to the other, either way
    /// the mouse went, and over lines the rest of the first, the whole of those
    /// between and the start of the last, whichever it went down on.
    #[test]
    fn a_sweep_goes_line_by_line() {
        let page = page(&["abcdefgh", "ijklmnop", "qrstuvwx"]);
        assert_eq!(swept(at(3, 1), at(5, 1), &page), "bcd");
        assert_eq!(swept(at(5, 1), at(3, 1), &page), "bcd");
        assert_eq!(swept(at(4, 1), at(4, 1), &page), "c");
        assert_eq!(swept(at(7, 1), at(4, 3), &page), "fgh\nijklmnop\nqrs");
        assert_eq!(swept(at(4, 3), at(7, 1), &page), "fgh\nijklmnop\nqrs");
    }

    /// Past the sidebar's columns or its lines, it goes as far as they do.
    #[test]
    fn a_sweep_stays_within_the_sidebar() {
        let page = page(&["abcdefghijklmnop", "qrstuvwxyz"]);
        assert_eq!(swept(at(11, 1), at(90, 1), &page), "j");
        assert_eq!(swept(at(11, 1), at(0, 2), &page), "j\nq");
        assert_eq!(swept(at(3, 2), at(3, 90), &page), "rstuvwxyz");
    }

    /// Reversed are the cells it takes, however long the lines are, and no others:
    /// a line's rest on all but the last, and no cell past the sidebar's columns.
    #[test]
    fn a_sweep_shows_reversed_on_the_cells_it_takes() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 16, 8));
        Sweep::new(at(9, 2), at(4, 4), WITHIN).draw(&mut buffer, WITHIN);
        let reversed = |y: u16| -> Vec<u16> {
            let on = |x: &u16| buffer[(*x, y)].modifier.contains(Modifier::REVERSED);
            (0..16).filter(on).collect()
        };
        assert_eq!(reversed(1), [0u16; 0]);
        assert_eq!(reversed(2), [9, 10, 11]);
        assert_eq!(reversed(3), [2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(reversed(4), [2, 3, 4]);
        assert_eq!(reversed(5), [0u16; 0]);
    }

    /// The blanks at the end of each line go, those the line draws and those it
    /// leaves undrawn, and the blank lines around it all, as a paste into a shell
    /// would run what ends in one. What is between stays, and so do the blanks
    /// a line starts with. Nothing but blanks is nothing to copy.
    #[test]
    fn the_blanks_at_the_ends_go() {
        let page = page(&["", "  ab   ", "", " cd", "    "]);
        assert_eq!(swept(at(2, 2), at(11, 2), &page), "  ab");
        assert_eq!(swept(at(2, 1), at(11, 5), &page), "  ab\n\n cd");
        assert_eq!(swept(at(5, 5), at(8, 5), &page), "");
    }

    /// A line that goes on from the one above joins it as the note was before it
    /// wrapped, a blank at the end of the first and all, and without the space
    /// the second starts with. A line that does not go on stays a line, though a
    /// note before it fills its own to the end.
    #[test]
    fn a_note_wrapped_comes_out_as_one_line() {
        let page = Page {
            goes_on: vec![1, 2],
            ..page(&[" lunetad: ", " stopped a", " nswering", " next note"])
        };
        assert_eq!(swept(at(2, 1), at(11, 4), &page), " lunetad: stopped answering\n next note");
        // From the middle of it, or stopping short of its end.
        assert_eq!(swept(at(3, 2), at(5, 3), &page), "stopped answ");
        assert_eq!(swept(at(5, 1), at(11, 1), &page), "netad:");
    }

    /// A `…` stands for what its cut took, where the sweep has it, and without it
    /// the text is what shows.
    #[test]
    fn the_end_of_a_cut_comes_out_as_what_it_took() {
        let mut cuts = Spots::default();
        cuts.add(1, 6..7, "-branch-name".to_owned());
        let page = Page { cuts: cuts.placed(WITHIN), ..page(&["", "  long…", ""]) };
        assert_eq!(swept(at(4, 2), at(8, 2), &page), "long-branch-name");
        assert_eq!(swept(at(6, 2), at(11, 2), &page), "ng-branch-name");
        assert_eq!(swept(at(4, 2), at(7, 2), &page), "long");
        assert_eq!(swept(at(8, 2), at(8, 2), &page), "-branch-name");
    }

    /// Cells count as they are drawn, wide characters as two, and a wide
    /// character is there by its first cell, as that alone says how it shows.
    #[test]
    fn a_wide_character_is_there_by_its_first_cell() {
        let page = page(&["日本語x"]);
        assert_eq!(swept(at(8, 1), at(8, 1), &page), "x");
        assert_eq!(swept(at(4, 1), at(5, 1), &page), "本");
        assert_eq!(swept(at(5, 1), at(6, 1), &page), "語");
        assert_eq!(swept(at(5, 1), at(5, 1), &page), "");
    }

    /// A sweep of one cell takes what ratatui drew on it, whatever counts how
    /// wide: past a wide character, and past a halfwidth letter with its sound
    /// mark, to which ratatui gives a cell more than `unicode_width` does.
    #[test]
    fn a_sweep_reads_each_cell_as_ratatui_drew_it() {
        let page = page(&["\u{ff8a}\u{ff9e}xy日z"]);
        let mut buffer = Buffer::empty(WITHIN);
        buffer.set_line(WITHIN.x, WITHIN.y, &page.lines[0], WITHIN.width);
        for x in WITHIN.left()..WITHIN.right() {
            let drawn = buffer[(x, WITHIN.y)].symbol().trim_end().to_owned();
            assert_eq!(swept(at(x, WITHIN.y), at(x, WITHIN.y), &page), drawn, "cell {x}");
        }
    }
}
