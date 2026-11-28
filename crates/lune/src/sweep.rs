use std::collections::BTreeMap;
use std::ops::Range;

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::spots::Spots;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Tight,
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Continuation {
    pub join: Join,
    pub indent: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutAt {
    pub line: usize,
    pub column: usize,
    pub took: String,
}

#[derive(Debug, Default)]
pub struct Page {
    pub lines: Vec<Line<'static>>,
    pub joins: BTreeMap<usize, Continuation>,
    pub cuts: Spots<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sweep {
    from: Position,
    to: Position,
}

impl Sweep {
    pub fn new(from: Position, to: Position, within: Rect) -> Sweep {
        assert!(!within.is_empty(), "a sweep starts inside a non-empty area");
        let sweep = Sweep {
            from: clamp(from, within),
            to: clamp(to, within),
        };
        assert!(within.contains(sweep.from), "sweep start falls inside area");
        assert!(within.contains(sweep.to), "sweep end falls inside area");
        sweep
    }

    pub fn reach(&mut self, to: Position, within: Rect) {
        assert!(
            !within.is_empty(),
            "a sweep reaches inside a non-empty area"
        );
        self.to = clamp(to, within);
        assert!(within.contains(self.to), "sweep end falls inside area");
    }

    pub fn last_y(self) -> u16 {
        self.ends().1.y
    }

    pub fn ends(self) -> (Position, Position) {
        let ends = if (self.from.y, self.from.x) <= (self.to.y, self.to.x) {
            (self.from, self.to)
        } else {
            (self.to, self.from)
        };
        assert!(
            (ends.0.y, ends.0.x) <= (ends.1.y, ends.1.x),
            "sweep ends are ordered top-left to bottom-right"
        );
        ends
    }

    fn columns(self, y: u16, within: Rect) -> Range<u16> {
        let (first, last) = self.ends();
        let start = if y == first.y { first.x } else { within.left() };
        let end = if y == last.y {
            last.x.saturating_add(1)
        } else {
            within.right()
        };
        start..end
    }

    pub fn draw(self, buffer: &mut Buffer, within: Rect) {
        let (first, last) = self.ends();
        for y in first.y..=last.y {
            let columns = self.columns(y, within);
            let cells = Rect::new(
                columns.start,
                y,
                columns.end.saturating_sub(columns.start),
                1,
            );
            let reversed = Style::new().add_modifier(Modifier::REVERSED);
            buffer.set_style(cells.intersection(within), reversed);
        }
    }

    pub fn text(self, page: &Page, within: Rect) -> String {
        assert!(!within.is_empty(), "a swept page has cells");
        let (first, last) = self.ends();
        assert!(first.y <= last.y, "sweep rows are ordered");
        let join = |line: usize| page.joins.get(&line).copied();
        let mut text = String::new();
        for y in first.y..=last.y {
            let line = usize::from(y.saturating_sub(within.y));
            let mut columns = self.columns(y, within);
            if y > first.y
                && let Some(continuation) = join(line)
            {
                assert!(
                    continuation.indent <= within.width,
                    "a continuation indent fits within the page width"
                );
                columns.start = columns.start.saturating_add(continuation.indent);
            }
            let said = page.lines.get(line).map_or_else(String::new, |drawn| {
                read(drawn, within.x, columns, |x| {
                    page.cuts.at(Position::new(x, y))
                })
            });
            if y < last.y {
                match join(line + 1).map(|continuation| continuation.join) {
                    Some(Join::Tight) => text.push_str(&said),
                    Some(Join::Space) => {
                        text.push_str(&said);
                        if !said.is_empty() {
                            text.push(' ');
                        }
                    }
                    None => {
                        text.push_str(said.trim_end());
                        text.push('\n');
                    }
                }
            } else {
                text.push_str(said.trim_end());
                text.push('\n');
            }
        }
        text.trim_start_matches('\n').trim_end().to_owned()
    }
}

pub fn clamp(at: Position, within: Rect) -> Position {
    assert!(!within.is_empty(), "a sweep area has at least one cell");
    let last = |start: u16, length: u16| start.saturating_add(length.saturating_sub(1));
    let clamped = Position::new(
        at.x.clamp(within.x, last(within.x, within.width)),
        at.y.clamp(within.y, last(within.y, within.height)),
    );
    assert!(
        within.contains(clamped),
        "a clamped position falls inside its area"
    );
    clamped
}

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

pub fn cells(text: &str) -> usize {
    let span = Span::raw(text);
    span.styled_graphemes(Style::new())
        .map(|it| usize::from(it.symbol.cell_width()))
        .sum()
}

pub fn line_cells(line: &Line<'_>) -> usize {
    line.styled_graphemes(Style::new())
        .map(|it| usize::from(it.symbol.cell_width()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITHIN: Rect = Rect {
        x: 2,
        y: 1,
        width: 10,
        height: 6,
    };

    fn at(x: u16, y: u16) -> Position {
        Position::new(x, y)
    }

    fn page(lines: &[&'static str]) -> Page {
        Page {
            lines: lines.iter().map(|line| Line::raw(*line)).collect(),
            ..Page::default()
        }
    }

    fn swept(from: Position, to: Position, page: &Page) -> String {
        Sweep::new(from, to, WITHIN).text(page, WITHIN)
    }

    #[test]
    fn a_sweep_goes_line_by_line() {
        let page = page(&["abcdefgh", "ijklmnop", "qrstuvwx"]);
        assert_eq!(swept(at(3, 1), at(5, 1), &page), "bcd");
        assert_eq!(swept(at(5, 1), at(3, 1), &page), "bcd");
        assert_eq!(swept(at(4, 1), at(4, 1), &page), "c");
        assert_eq!(swept(at(7, 1), at(4, 3), &page), "fgh\nijklmnop\nqrs");
        assert_eq!(swept(at(4, 3), at(7, 1), &page), "fgh\nijklmnop\nqrs");
    }

    #[test]
    fn a_sweep_stays_within_the_sidebar() {
        let page = page(&["abcdefghijklmnop", "qrstuvwxyz"]);
        assert_eq!(swept(at(11, 1), at(90, 1), &page), "j");
        assert_eq!(swept(at(11, 1), at(0, 2), &page), "j\nq");
        assert_eq!(swept(at(3, 2), at(3, 90), &page), "rstuvwxyz");
    }

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

    #[test]
    fn the_blanks_at_the_ends_go() {
        let page = page(&["", "  ab   ", "", " cd", "    "]);
        assert_eq!(swept(at(2, 2), at(11, 2), &page), "  ab");
        assert_eq!(swept(at(2, 1), at(11, 5), &page), "  ab\n\n cd");
        assert_eq!(swept(at(5, 5), at(8, 5), &page), "");
    }

    #[test]
    fn a_note_wrapped_comes_out_as_one_line() {
        let tight = Continuation {
            join: Join::Tight,
            indent: 1,
        };
        let page = Page {
            joins: BTreeMap::from([(1, tight), (2, tight)]),
            ..page(&[" luned: ", " stopped a", " nswering", " next note"])
        };
        assert_eq!(
            swept(at(2, 1), at(11, 4), &page),
            " luned: stopped answering\n next note"
        );
        assert_eq!(swept(at(3, 2), at(5, 3), &page), "stopped answ");
        assert_eq!(swept(at(5, 1), at(11, 1), &page), "ned:");
    }

    #[test]
    fn word_and_hard_wrapped_lines_join_with_space_or_tight() {
        let space = Continuation {
            join: Join::Space,
            indent: 0,
        };
        let tight = Continuation {
            join: Join::Tight,
            indent: 0,
        };
        let page = Page {
            joins: BTreeMap::from([(1, space), (2, tight), (3, space)]),
            ..page(&["see", "https://ex", "ample.com", "now", "next"])
        };
        assert_eq!(
            swept(at(2, 1), at(5, 5), &page),
            "see https://example.com now\nnext"
        );
        assert_eq!(swept(at(2, 2), at(10, 3), &page), "https://example.com");
        assert_eq!(swept(at(8, 1), at(6, 2), &page), "https");
    }

    #[test]
    fn the_end_of_a_cut_comes_out_as_what_it_took() {
        let mut cuts = Spots::default();
        cuts.add(1, 6..7, "-branch-name".to_owned());
        let page = Page {
            cuts: cuts.placed(WITHIN),
            ..page(&["", "  long…", ""])
        };
        assert_eq!(swept(at(4, 2), at(8, 2), &page), "long-branch-name");
        assert_eq!(swept(at(6, 2), at(11, 2), &page), "ng-branch-name");
        assert_eq!(swept(at(4, 2), at(7, 2), &page), "long");
        assert_eq!(swept(at(8, 2), at(8, 2), &page), "-branch-name");
    }

    #[test]
    fn a_wide_character_is_there_by_its_first_cell() {
        let page = page(&["日本語x"]);
        assert_eq!(swept(at(8, 1), at(8, 1), &page), "x");
        assert_eq!(swept(at(4, 1), at(5, 1), &page), "本");
        assert_eq!(swept(at(5, 1), at(6, 1), &page), "語");
        assert_eq!(swept(at(5, 1), at(5, 1), &page), "");
    }

    #[test]
    fn a_sweep_reads_each_cell_as_ratatui_drew_it() {
        let page = page(&["\u{ff8a}\u{ff9e}xy日z"]);
        let mut buffer = Buffer::empty(WITHIN);
        buffer.set_line(WITHIN.x, WITHIN.y, &page.lines[0], WITHIN.width);
        for x in WITHIN.left()..WITHIN.right() {
            let drawn = buffer[(x, WITHIN.y)].symbol().trim_end().to_owned();
            assert_eq!(
                swept(at(x, WITHIN.y), at(x, WITHIN.y), &page),
                drawn,
                "cell {x}"
            );
        }
    }
}
