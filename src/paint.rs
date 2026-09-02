//! Puts a styled row of the frame on the wire.
//!
//! zellij's `text;` component wants a row as comma-separated decimal bytes,
//! followed by one comma-separated index list per style level.
//! `zellij_tile::Text` builds that by allocating a `String` for every single
//! byte and every single index — 160 allocations for one 58-column row, some
//! 9,000 for a frame, ten frames a second while a spinner turns.
//!
//! `Painted` holds the same content and the same index lists and writes the
//! same bytes into one reused buffer. `same_bytes_as_zellij_tile` in the tests
//! below pins the output to what `Text` would have produced.

use std::cell::RefCell;
use std::ops::{Bound, Range, RangeBounds};

const DIM: usize = 4;
const UNBOLD: usize = 5;
const ERROR: usize = 6;

/// A row, and which of its characters carry which style level.
#[derive(Default)]
pub struct Painted {
    text: String,
    chars: usize,
    selected: bool,
    indices: Vec<Vec<usize>>,
}

impl Painted {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let chars = text.chars().count();
        Painted { text, chars, selected: false, indices: Vec::new() }
    }

    #[cfg(test)]
    pub fn content(&self) -> &str {
        &self.text
    }

    pub fn selected(mut self) -> Self {
        self.selected = true;
        self
    }

    pub fn color_indices(mut self, level: usize, indices: Vec<usize>) -> Self {
        self.level(level).extend(indices);
        self
    }

    pub fn color_range<R: RangeBounds<usize>>(mut self, level: usize, range: R) -> Self {
        let range = self.bounds(range);
        self.level(level).extend(range);
        self
    }

    pub fn color_substring(mut self, level: usize, substr: &str) -> Self {
        let mut from = 0;
        while let Some(at) = self.text[from..].find(substr) {
            let at = from + at;
            let start = self.text[..at].chars().count();
            let end = start + substr.chars().count();
            self.level(level).extend(start..end);
            from = at + substr.len();
        }
        self
    }

    pub fn dim_indices(self, indices: Vec<usize>) -> Self {
        self.color_indices(DIM, indices)
    }

    pub fn dim_all(self) -> Self {
        self.color_range(DIM, ..)
    }

    pub fn unbold_indices(self, indices: Vec<usize>) -> Self {
        self.color_indices(UNBOLD, indices)
    }

    pub fn unbold_range<R: RangeBounds<usize>>(self, range: R) -> Self {
        self.color_range(UNBOLD, range)
    }

    pub fn error_color_range<R: RangeBounds<usize>>(self, range: R) -> Self {
        self.color_range(ERROR, range)
    }

    /// Grows the index lists to cover `level`. The empty lists in between are
    /// part of the payload: each one is a `$` the component counts off to know
    /// which level the next list belongs to.
    fn level(&mut self, level: usize) -> &mut Vec<usize> {
        if self.indices.len() <= level {
            self.indices.resize_with(level + 1, Vec::new);
        }
        &mut self.indices[level]
    }

    fn bounds<R: RangeBounds<usize>>(&self, range: R) -> Range<usize> {
        let start = match range.start_bound() {
            Bound::Unbounded => 0,
            Bound::Included(at) | Bound::Excluded(at) => *at,
        };
        let end = match range.end_bound() {
            Bound::Unbounded => self.chars,
            Bound::Included(at) => *at + 1,
            Bound::Excluded(at) => *at,
        };
        start..end
    }

    fn write(&self, out: &mut String) {
        if self.selected {
            out.push('x');
        }
        for level in &self.indices {
            for (nth, index) in level.iter().enumerate() {
                if nth > 0 {
                    out.push(',');
                }
                digits(out, *index);
            }
            out.push('$');
        }
        for (nth, byte) in self.text.as_bytes().iter().enumerate() {
            if nth > 0 {
                out.push(',');
            }
            digits(out, usize::from(*byte));
        }
    }

    /// The payload on its own, in a fresh `String`. `print_at` writes through a
    /// reused buffer instead; this is only here for the tests below.
    #[cfg(test)]
    fn serialize(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

thread_local! {
    /// One buffer for the whole run. A frame writes some 150 rows through it.
    static WIRE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Prints `painted` at `x`, `y` in a box `width` columns wide.
pub fn print_at(painted: &Painted, x: usize, y: usize, width: usize) {
    WIRE.with_borrow_mut(|out| {
        out.clear();
        out.push_str("\u{1b}Pztext;");
        digits(out, x);
        out.push('/');
        digits(out, y);
        out.push('/');
        digits(out, width);
        out.push_str("/;");
        painted.write(out);
        out.push_str("\u{1b}\\");
        print!("{}", out);
    });
}

fn digits(out: &mut String, mut n: usize) {
    if n == 0 {
        out.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut at = buf.len();
    while n > 0 {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.push_str(std::str::from_utf8(&buf[at..]).expect("decimal digits are ascii"));
}

#[cfg(test)]
mod tests {
    use zellij_tile::prelude::Text;

    use super::*;

    #[test]
    fn same_bytes_as_zellij_tile() {
        let row = " 日本語版 session ─ [R] ";
        let cases: Vec<(Painted, Text)> = vec![
            (Painted::new(row), Text::new(row)),
            (Painted::new(row).dim_all(), Text::new(row).dim_all()),
            (
                Painted::new(row).unbold_indices(vec![1, 2, 3, 9]),
                Text::new(row).unbold_indices(vec![1, 2, 3, 9]),
            ),
            (
                Painted::new(row).dim_indices(vec![0, 4]).unbold_indices(vec![2]),
                Text::new(row).dim_indices(vec![0, 4]).unbold_indices(vec![2]),
            ),
            (
                Painted::new(row).color_range(1, 3..8).color_indices(3, vec![4, 6]),
                Text::new(row).color_range(1, 3..8).color_indices(3, vec![4, 6]),
            ),
            (
                Painted::new(row).unbold_range(2..5).color_range(0, 2..5),
                Text::new(row).unbold_range(2..5).color_range(0, 2..5),
            ),
            (
                Painted::new(row).error_color_range(1..4),
                Text::new(row).error_color_range(1..4),
            ),
            (
                Painted::new(row).color_substring(3, "session"),
                Text::new(row).color_substring(3, "session"),
            ),
            (
                Painted::new(row).dim_all().selected(),
                Text::new(row).dim_all().selected(),
            ),
            (Painted::new("").dim_all(), Text::new("").dim_all()),
            (
                Painted::new(row).color_range(2, ..).color_range(1, 0..0),
                Text::new(row).color_range(2, ..).color_range(1, 0..0),
            ),
        ];
        for (nth, (painted, text)) in cases.iter().enumerate() {
            assert_eq!(painted.serialize(), text.serialize(), "case {nth}");
            assert_eq!(painted.content(), text.content(), "case {nth}");
        }
    }

    #[test]
    fn digits_spell_out_every_number_it_will_ever_see() {
        for n in [0, 1, 9, 10, 99, 100, 255, 1000, usize::MAX] {
            let mut out = String::new();
            digits(&mut out, n);
            assert_eq!(out, n.to_string());
        }
    }

    #[test]
    fn a_substring_is_coloured_wherever_it_appears() {
        let text = "ab ab";
        assert_eq!(
            Painted::new(text).color_substring(0, "ab").serialize(),
            Text::new(text).color_substring(0, "ab").serialize()
        );
    }
}
