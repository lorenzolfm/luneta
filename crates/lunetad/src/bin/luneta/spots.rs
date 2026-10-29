//! What a click does where luneta drew something it takes one on: an entry
//! of the menu or the legend, a key a line of keys names, a skill of the
//! search over them, a row of the picker. Each says, as it draws, what it
//! drew where, and a click goes by what it said, so that what you see and
//! what a click hits never disagree: to what drew it, while that has the
//! mouse still, and to the row or the skill it drew there, wherever that is
//! now. The spot under the pointer is lit, for you to see what a click there
//! hits before you click.

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::gruvbox;
use crate::menu::Choice;
use crate::picker::Pick;

/// The background of the spot under the pointer, where the spot has none of
/// its own: the softer of the highlight's two, which a row the highlight is
/// on keeps.
pub const POINTED: Color = gruvbox::BG1;

/// How soon after a click on a cell another there, or next to it, is the
/// next of a burst, as desktops have a double click by default.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// Spots, each where it is, and what a click on it does: on luneta's
/// terminal once drawn there, or before, on the lines a box draws, by the
/// line and the columns along it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spots<S>(Vec<(Rect, S)>);

impl<S> Default for Spots<S> {
    fn default() -> Self {
        Spots(Vec::new())
    }
}

impl<S> Spots<S> {
    /// Puts `spot` on the columns `columns` of the line `line`.
    pub fn add(&mut self, line: usize, columns: Range<usize>, spot: S) {
        let (Ok(y), Ok(x)) = (u16::try_from(line), u16::try_from(columns.start)) else {
            return;
        };
        let width = u16::try_from(columns.len()).unwrap_or(u16::MAX);
        self.0.push((Rect::new(x, y, width, 1), spot));
    }

    /// Those of these spots on their first `lines` lines.
    pub fn first(mut self, lines: usize) -> Spots<S> {
        self.0.retain(|(area, _)| usize::from(area.y) < lines);
        self
    }

    /// These spots `lines` lines further down, as their lines are drawn under
    /// that many others.
    pub fn down(self, lines: usize) -> Spots<S> {
        self.moved(0, u16::try_from(lines).unwrap_or(u16::MAX))
    }

    /// These spots `right` columns further right, and `down` lines further
    /// down.
    fn moved(self, right: u16, down: u16) -> Spots<S> {
        let moved = |(area, spot): (Rect, S)| {
            let x = area.x.saturating_add(right);
            (Rect { x, y: area.y.saturating_add(down), ..area }, spot)
        };
        Spots(self.0.into_iter().map(moved).collect())
    }

    /// These spots on their lines drawn into `area` from its top left: those
    /// that land in it, cut to it.
    pub fn placed(self, area: Rect) -> Spots<S> {
        let Spots(moved) = self.moved(area.x, area.y);
        let cut = moved.into_iter().map(|(spot_area, spot)| (spot_area.intersection(area), spot));
        Spots(cut.filter(|(area, _)| !area.is_empty()).collect())
    }

    pub fn extend(&mut self, spots: Spots<S>) {
        self.0.extend(spots.0);
    }

    /// These spots, each doing what `does` makes of what it did.
    pub fn map<T>(self, does: impl Fn(S) -> T) -> Spots<T> {
        Spots(self.0.into_iter().map(|(area, spot)| (area, does(spot))).collect())
    }

    /// The spot at `at`, if one is.
    pub fn at(&self, at: Position) -> Option<&S> {
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

/// What has the mouse drew last, and its spots, where they are on luneta's
/// terminal. A click on one goes to what drew it while that has the mouse
/// still, as what zellij answers a menu or a picker goes to it by its
/// number, and to nothing that took the mouse since.
#[derive(Debug, Default)]
pub enum Drawn {
    /// Nothing that takes a click: a brief, the ask box or a note, or the
    /// sidebar collapsed, or with its search under the rows, or with nothing
    /// there, or a box that had no room.
    #[default]
    Nothing,
    /// The legend at the bottom of the sidebar.
    Legend(Spots<KeyEvent>),
    /// The menu numbered `number`: floating over zellij in the box `area`, or
    /// at the bottom of the sidebar, in none.
    Menu { number: u64, area: Option<Rect>, spots: Spots<Choice> },
    /// The picker numbered `number`, in the box `area`.
    Picker { number: u64, area: Rect, spots: Spots<Pick> },
}

/// A spot a click hits, with what drew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Legend(KeyEvent),
    /// On the menu numbered so.
    Menu(u64, Choice),
    /// On the picker numbered so.
    Picker(u64, Pick),
}

impl Drawn {
    /// The box drawn over zellij, the menu's or the picker's, if one was.
    pub fn boxed(&self) -> Option<Rect> {
        match self {
            Drawn::Menu { area, .. } => *area,
            Drawn::Picker { area, .. } => Some(*area),
            Drawn::Nothing | Drawn::Legend(_) => None,
        }
    }

    /// The spot a click at `at` hits, if one is there.
    pub fn at(&self, at: Position) -> Option<Hit> {
        match self {
            Drawn::Nothing => None,
            Drawn::Legend(spots) => spots.at(at).copied().map(Hit::Legend),
            Drawn::Menu { number, spots, .. } => {
                spots.at(at).cloned().map(|choice| Hit::Menu(*number, choice))
            },
            Drawn::Picker { number, spots, .. } => {
                spots.at(at).cloned().map(|pick| Hit::Picker(*number, pick))
            },
        }
    }

    /// Lights the spot under `pointer` on `buffer`, as `Spots::light` does.
    pub fn light(&self, buffer: &mut Buffer, pointer: Position) {
        match self {
            Drawn::Nothing => {},
            Drawn::Legend(spots) => spots.light(buffer, pointer),
            Drawn::Menu { spots, .. } => spots.light(buffer, pointer),
            Drawn::Picker { spots, .. } => spots.light(buffer, pointer),
        }
    }

    /// What drew, with its spots on lines drawn `lines` lines further down,
    /// into `area` from its top left, as `Spots::placed` places them.
    pub fn placed(self, lines: usize, area: Rect) -> Drawn {
        match self {
            Drawn::Nothing => Drawn::Nothing,
            Drawn::Legend(spots) => Drawn::Legend(spots.down(lines).placed(area)),
            Drawn::Menu { number, area: boxed, spots } => {
                Drawn::Menu { number, area: boxed, spots: spots.down(lines).placed(area) }
            },
            Drawn::Picker { number, area: boxed, spots } => {
                Drawn::Picker { number, area: boxed, spots: spots.down(lines).placed(area) }
            },
        }
    }
}

/// Clicks that come fast on one cell after one that changed what is drawn
/// there: a spot's, or the one that opened the sidebar collapsed. None of
/// them is a click on what the first drew there, and the burst lasts from
/// the last of them, so that the third click of a triple click is none
/// either. A hand that clicks twice moves a little, as desktops allow it a
/// few pixels, which can take the pointer over a cell's edge: a click on a
/// cell next to the last one's is one more of the burst too, and the burst
/// goes on from there.
#[derive(Clone, Copy, Debug)]
pub struct Burst {
    at: Position,
    last: Instant,
}

impl Burst {
    /// The burst a click at `at`, `now`, starts.
    pub fn new(at: Position, now: Instant) -> Burst {
        Burst { at, last: now }
    }

    /// Whether a press at `at`, `now`, is one more of the burst, which then
    /// lasts from it, and from where it is.
    pub fn takes(&mut self, at: Position, now: Instant) -> bool {
        let near = self.at.x.abs_diff(at.x) <= 1 && self.at.y.abs_diff(at.y) <= 1;
        let more = near && now.saturating_duration_since(self.last) < DOUBLE_CLICK;
        if more {
            self.at = at;
            self.last = now;
        }
        more
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    /// Spots put on the lines of a box land where those lines are drawn,
    /// cut to the room they have there.
    #[test]
    fn spots_land_where_their_lines_are_drawn() {
        let mut spots = Spots::default();
        spots.add(0, 0..6, key('a'));
        spots.add(1, 2..10, key('r'));
        spots.add(4, 0..6, key('b'));
        let placed = spots.first(4).down(1).placed(Rect::new(10, 20, 8, 3));
        assert_eq!(placed.at(Position::new(10, 21)), Some(&key('a')));
        assert_eq!(placed.at(Position::new(15, 21)), Some(&key('a')));
        assert_eq!(placed.at(Position::new(16, 21)), None);
        assert_eq!(placed.at(Position::new(12, 22)), Some(&key('r')));
        // Cut to the area's right, and its bottom.
        assert_eq!(placed.at(Position::new(17, 22)), Some(&key('r')));
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

    /// A click hits a spot with what drew it, and the box over zellij is the
    /// menu's floating there, or the picker's, and none at the bottom of the
    /// sidebar.
    #[test]
    fn a_click_hits_a_spot_with_what_drew_it() {
        let mut spots = Spots::default();
        spots.add(0, 0..4, key('d'));
        let area = Rect::new(0, 0, 4, 1);
        let at = Position::new(1, 0);
        let legend = Drawn::Legend(spots.clone());
        assert_eq!(legend.at(at), Some(Hit::Legend(key('d'))));
        assert_eq!(legend.boxed(), None);
        let menu = |area| Drawn::Menu { number: 3, area, spots: spots.clone().map(Choice::Key) };
        assert_eq!(menu(None).at(at), Some(Hit::Menu(3, Choice::Key(key('d')))));
        assert_eq!(menu(None).boxed(), None);
        assert_eq!(menu(Some(area)).boxed(), Some(area));
        assert_eq!(Drawn::Nothing.at(at), None);
    }

    /// Every click on the cell soon after the one before it is one more of
    /// the burst: the second of a double click, and the third of a triple.
    /// One later than that, or two cells off, is a click.
    #[test]
    fn every_fast_click_on_the_cell_is_one_more_of_the_burst() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let at = Position::new(4, 14);
        let mut burst = Burst::new(at, start);
        assert!(burst.takes(at, after(150)));
        assert!(burst.takes(at, after(300)));
        // 450ms after the last, whatever the time since the first.
        assert!(burst.takes(at, after(750)));
        assert!(!burst.takes(at, after(1300)));
        for off in [Position::new(6, 14), Position::new(2, 14), Position::new(4, 16)] {
            let mut burst = Burst::new(at, start);
            assert!(!burst.takes(off, after(100)), "{off:?}");
        }
    }

    /// A fast click on a cell next to the last one's, as the hand moved a
    /// little between them, is one more of the burst: on the line of the
    /// entry clicked, it would hit the one that took its place, and on the
    /// line under it, the one under that. The burst goes on from where the
    /// last of it is.
    #[test]
    fn a_fast_click_next_to_the_last_is_one_more_of_the_burst() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let mut burst = Burst::new(Position::new(4, 14), start);
        assert!(burst.takes(Position::new(5, 14), after(150)));
        assert!(burst.takes(Position::new(5, 15), after(300)));
        // Next to the last, though two columns from the first.
        assert!(burst.takes(Position::new(6, 14), after(450)));
    }
}
