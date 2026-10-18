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
//!
//! The component dims a character only while it draws it bold, so a run in
//! gray and not bold is written past it, as plain escape codes.

use std::cell::RefCell;
use std::ops::{Bound, Range, RangeBounds};

use unicode_width::UnicodeWidthChar;

const DIM: usize = 4;
const UNBOLD: usize = 5;
const ERROR: usize = 6;
/// zellij's success colour: green in most themes, the level past the error.
const SUCCESS: usize = 7;

/// A colour zellij's text component draws a run in: one of the theme's four
/// emphasis levels, or its success colour. Dim, unbold and the error colour
/// have calls of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Level0,
    Level1,
    Level2,
    Level3,
    Success,
}

impl Colour {
    /// The index list zellij reads this colour from.
    fn slot(self) -> usize {
        match self {
            Colour::Level0 => 0,
            Colour::Level1 => 1,
            Colour::Level2 => 2,
            Colour::Level3 => 3,
            Colour::Success => SUCCESS,
        }
    }
}

/// A row, and which of its characters carry which style level.
#[derive(Default)]
pub struct Painted {
    text: String,
    chars: usize,
    selected: bool,
    indices: Vec<Vec<usize>>,
    /// The characters written again over the component in the terminal's
    /// gray, in order.
    gray: Vec<usize>,
}

impl Painted {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let chars = text.chars().count();
        Painted { text, chars, selected: false, indices: Vec::new(), gray: Vec::new() }
    }

    #[cfg(test)]
    pub fn content(&self) -> &str {
        &self.text
    }

    /// The colour zellij draws the character at `index` in, as its text
    /// component picks: the success colour over the highest of the four
    /// emphasis levels that holds it.
    #[cfg(test)]
    pub fn colour_at(&self, index: usize) -> Option<Colour> {
        [Colour::Success, Colour::Level3, Colour::Level2, Colour::Level1, Colour::Level0]
            .into_iter()
            .find(|colour| self.indices.get(colour.slot()).is_some_and(|at| at.contains(&index)))
    }

    #[cfg(test)]
    pub fn is_dim_at(&self, index: usize) -> bool {
        self.indices.get(DIM).is_some_and(|at| at.contains(&index))
    }

    #[cfg(test)]
    pub fn is_bold_at(&self, index: usize) -> bool {
        !self.indices.get(UNBOLD).is_some_and(|at| at.contains(&index))
    }

    #[cfg(test)]
    pub fn is_gray_at(&self, index: usize) -> bool {
        self.gray.contains(&index)
    }

    #[cfg(test)]
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn selected(mut self) -> Self {
        self.selected = true;
        self
    }

    pub fn color_indices(self, colour: Colour, indices: Vec<usize>) -> Self {
        self.slot_indices(colour.slot(), indices)
    }

    pub fn color_range<R: RangeBounds<usize>>(self, colour: Colour, range: R) -> Self {
        self.slot_range(colour.slot(), range)
    }

    pub fn color_substring(mut self, colour: Colour, substr: &str) -> Self {
        let mut from = 0;
        while let Some(at) = self.text[from..].find(substr) {
            let at = from + at;
            let start = self.text[..at].chars().count();
            let end = start + substr.chars().count();
            self.slot(colour.slot()).extend(start..end);
            from = at + substr.len();
        }
        self
    }

    pub fn dim_indices(self, indices: Vec<usize>) -> Self {
        self.slot_indices(DIM, indices)
    }

    pub fn dim_all(self) -> Self {
        self.slot_range(DIM, ..)
    }

    pub fn unbold_indices(self, indices: Vec<usize>) -> Self {
        self.slot_indices(UNBOLD, indices)
    }

    pub fn unbold_range<R: RangeBounds<usize>>(self, range: R) -> Self {
        self.slot_range(UNBOLD, range)
    }

    pub fn error_color_range<R: RangeBounds<usize>>(self, range: R) -> Self {
        self.slot_range(ERROR, range)
    }

    pub fn gray_indices(mut self, indices: Vec<usize>) -> Self {
        self.gray.extend(indices);
        self.gray.sort_unstable();
        self.gray.dedup();
        self
    }

    pub fn gray_range<R: RangeBounds<usize>>(self, range: R) -> Self {
        let range = self.bounds(range);
        self.gray_indices(range.collect())
    }

    fn slot_indices(mut self, slot: usize, indices: Vec<usize>) -> Self {
        self.slot(slot).extend(indices);
        self
    }

    fn slot_range<R: RangeBounds<usize>>(mut self, slot: usize, range: R) -> Self {
        let range = self.bounds(range);
        self.slot(slot).extend(range);
        self
    }

    /// Grows the index lists to cover `slot`. The empty lists in between are
    /// part of the payload: each one is a `$` the component counts off to know
    /// which slot the next list belongs to.
    fn slot(&mut self, slot: usize) -> &mut Vec<usize> {
        if self.indices.len() <= slot {
            self.indices.resize_with(slot + 1, Vec::new);
        }
        &mut self.indices[slot]
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

    /// The gray characters of a row drawn at `x`, `y`, a run at a time, each
    /// at its column: neither bold nor dim, in the terminal's bright black.
    /// The component leaves the pen on its row's own style, so the runs keep
    /// the row's background, lit or not.
    fn write_gray(&self, out: &mut String, x: usize, y: usize) {
        if self.gray.is_empty() {
            return;
        }
        out.push_str("\u{1b}[22;90m");
        let mut gray = self.gray.iter().copied().peekable();
        let mut column = x;
        let mut open = false;
        for (index, character) in self.text.chars().enumerate() {
            let lit = gray.next_if_eq(&index).is_some();
            if lit && !open {
                out.push_str("\u{1b}[");
                digits(out, y + 1);
                out.push(';');
                digits(out, column + 1);
                out.push('H');
            }
            if lit {
                out.push(character);
            }
            open = lit;
            column += character.width().unwrap_or(0);
        }
        out.push_str("\u{1b}[m");
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
        wire(out, painted, x, y, width);
        print!("{out}");
    });
}

fn wire(out: &mut String, painted: &Painted, x: usize, y: usize, width: usize) {
    out.push_str("\u{1b}Pztext;");
    digits(out, x);
    out.push('/');
    digits(out, y);
    out.push('/');
    digits(out, width);
    out.push_str("/;");
    painted.write(out);
    out.push_str("\u{1b}\\");
    painted.write_gray(out, x, y);
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
                Painted::new(row)
                    .color_range(Colour::Level1, 3..8)
                    .color_indices(Colour::Level3, vec![4, 6]),
                Text::new(row).color_range(1, 3..8).color_indices(3, vec![4, 6]),
            ),
            (
                Painted::new(row).unbold_range(2..5).color_range(Colour::Level0, 2..5),
                Text::new(row).unbold_range(2..5).color_range(0, 2..5),
            ),
            (
                Painted::new(row).error_color_range(1..4),
                Text::new(row).error_color_range(1..4),
            ),
            (
                Painted::new(row).color_range(Colour::Success, 1..4),
                Text::new(row).success_color_range(1..4),
            ),
            (
                Painted::new(row).color_substring(Colour::Level3, "session"),
                Text::new(row).color_substring(3, "session"),
            ),
            (
                Painted::new(row).dim_all().selected(),
                Text::new(row).dim_all().selected(),
            ),
            (Painted::new("").dim_all(), Text::new("").dim_all()),
            (
                Painted::new(row).color_range(Colour::Level2, ..).color_range(Colour::Level1, 0..0),
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

    /// A gray run goes on the wire after the component, at its own column,
    /// counted in columns rather than characters.
    #[test]
    fn a_gray_run_is_written_over_the_component_at_its_column() {
        let painted = Painted::new("日 ab cd").gray_indices(vec![5, 2, 3, 6]);
        let mut out = String::new();
        wire(&mut out, &painted, 4, 7, 9);
        let component = format!("\u{1b}Pztext;4/7/9/;{}\u{1b}\\", painted.serialize());
        assert_eq!(
            out.strip_prefix(&component),
            Some("\u{1b}[22;90m\u{1b}[8;8Hab\u{1b}[8;11Hcd\u{1b}[m")
        );

        let mut out = String::new();
        wire(&mut out, &Painted::new("ab"), 0, 0, 2);
        assert!(!out.contains("\u{1b}["), "a row with no gray run is the component alone");
    }

    #[test]
    fn a_substring_is_coloured_wherever_it_appears() {
        let text = "ab ab";
        assert_eq!(
            Painted::new(text).color_substring(Colour::Level0, "ab").serialize(),
            Text::new(text).color_substring(0, "ab").serialize()
        );
    }
}
