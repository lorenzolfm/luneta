//! What a click does where luneta drew something it takes one on: an entry
//! of the menu or the legend, a key a line of keys names, a skill of the
//! search over them, a row of the picker. Each says, as it draws, what it
//! drew where, and a click goes by what it said, so that what you see and
//! what a click hits never disagree. The spot under the pointer is lit, for
//! you to see what a click there hits before you click.

use std::ops::Range;

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::gruvbox;

/// The background of the spot under the pointer, where the spot has none of
/// its own: the softer of the highlight's two, which a row the highlight is
/// on keeps.
pub const POINTED: Color = gruvbox::BG1;

/// What a click on a spot does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// Presses the key: an entry's, or one a line of keys names.
    Key(KeyEvent),
    /// Picks the row of a list numbered so among its rows, as `Enter` does
    /// with the highlight on it.
    Row(usize),
}

/// Spots, each where it is: on luneta's terminal once drawn there, or
/// before, on the lines a box draws, by the line and the columns along it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spots(Vec<(Rect, Spot)>);

impl Spots {
    /// Puts `spot` on the columns `columns` of the line `line`.
    pub fn add(&mut self, line: usize, columns: Range<usize>, spot: Spot) {
        let (Ok(y), Ok(x)) = (u16::try_from(line), u16::try_from(columns.start)) else {
            return;
        };
        let width = u16::try_from(columns.len()).unwrap_or(u16::MAX);
        self.0.push((Rect::new(x, y, width, 1), spot));
    }

    /// Those of these spots on their first `lines` lines.
    pub fn first(mut self, lines: usize) -> Spots {
        self.0.retain(|(area, _)| usize::from(area.y) < lines);
        self
    }

    /// These spots `lines` lines further down, as their lines are drawn under
    /// that many others.
    pub fn down(self, lines: usize) -> Spots {
        self.moved(0, u16::try_from(lines).unwrap_or(u16::MAX))
    }

    /// These spots `right` columns further right, and `down` lines further
    /// down.
    fn moved(self, right: u16, down: u16) -> Spots {
        let moved = |(area, spot): (Rect, Spot)| {
            let x = area.x.saturating_add(right);
            (Rect { x, y: area.y.saturating_add(down), ..area }, spot)
        };
        Spots(self.0.into_iter().map(moved).collect())
    }

    /// These spots on their lines drawn into `area` from its top left: those
    /// that land in it, cut to it.
    pub fn placed(self, area: Rect) -> Spots {
        let Spots(moved) = self.moved(area.x, area.y);
        let cut = moved.into_iter().map(|(spot_area, spot)| (spot_area.intersection(area), spot));
        Spots(cut.filter(|(area, _)| !area.is_empty()).collect())
    }

    pub fn extend(&mut self, spots: Spots) {
        self.0.extend(spots.0);
    }

    /// The spot at `at`, if one is.
    pub fn at(&self, at: Position) -> Option<&Spot> {
        self.0.iter().find(|(area, _)| area.contains(at)).map(|(_, spot)| spot)
    }

    /// Lights the spot under `pointer` on `buffer`, where the spot has no
    /// background of its own.
    pub fn light(&self, buffer: &mut Buffer, pointer: Position) {
        let Some((area, _)) = self.0.iter().find(|(area, _)| area.contains(pointer)) else {
            return;
        };
        for at in area.positions() {
            if let Some(cell) = buffer.cell_mut(at)
                && cell.bg == Color::Reset
            {
                cell.set_bg(POINTED);
            }
        }
    }
}

/// What a box drew, or the sidebar: where, and the spots in it, where they
/// are on luneta's terminal.
#[derive(Debug, Default)]
pub struct Drawn {
    pub area: Rect,
    pub spots: Spots,
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;

    fn key(c: char) -> Spot {
        Spot::Key(KeyEvent::from(KeyCode::Char(c)))
    }

    /// Spots put on the lines of a box land where those lines are drawn,
    /// cut to the room they have there.
    #[test]
    fn spots_land_where_their_lines_are_drawn() {
        let mut spots = Spots::default();
        spots.add(0, 0..6, key('a'));
        spots.add(1, 2..10, Spot::Row(3));
        spots.add(4, 0..6, key('b'));
        let placed = spots.first(4).down(1).placed(Rect::new(10, 20, 8, 3));
        assert_eq!(placed.at(Position::new(10, 21)), Some(&key('a')));
        assert_eq!(placed.at(Position::new(15, 21)), Some(&key('a')));
        assert_eq!(placed.at(Position::new(16, 21)), None);
        assert_eq!(placed.at(Position::new(12, 22)), Some(&Spot::Row(3)));
        // Cut to the area's right, and its bottom.
        assert_eq!(placed.at(Position::new(17, 22)), Some(&Spot::Row(3)));
        assert_eq!(placed.at(Position::new(18, 22)), None);
        assert_eq!(placed.at(Position::new(10, 25)), None);
        assert_eq!(placed.at(Position::new(10, 20)), None);
    }

    /// The pointer lights the spot it is on, but for what has a background
    /// of its own, and nothing off a spot.
    #[test]
    fn the_pointer_lights_the_spot_it_is_on() {
        let mut spots = Spots::default();
        spots.add(0, 0..4, key('a'));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        buffer[(1, 0)].set_bg(gruvbox::BG2);
        spots.light(&mut buffer, Position::new(5, 0));
        spots.light(&mut buffer, Position::new(0, 1));
        assert!(buffer.content().iter().all(|cell| cell.bg != POINTED));
        spots.light(&mut buffer, Position::new(2, 0));
        let lit: Vec<Color> = (0..6).map(|x| buffer[(x, 0)].bg).collect();
        assert_eq!(lit, [POINTED, gruvbox::BG2, POINTED, POINTED, Color::Reset, Color::Reset]);
    }
}
