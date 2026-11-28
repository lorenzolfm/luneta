use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::aloud::Button;
use crate::menu::Choice;
use crate::moonlight;
use crate::picker::Pick;
use crate::pull::Link;

pub const POINTED: Color = moonlight::SURFACE;

const DOUBLE_CLICK: Duration = Duration::from_millis(500);

const UNSEEN: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spots<S>(Vec<(Rect, S)>);

impl<S> Default for Spots<S> {
    fn default() -> Self {
        Spots(Vec::new())
    }
}

impl<S> Spots<S> {
    pub const fn empty() -> Self {
        Spots(Vec::new())
    }

    pub fn add(&mut self, line: usize, columns: Range<usize>, spot: S) {
        let (Ok(y), Ok(x)) = (u16::try_from(line), u16::try_from(columns.start)) else {
            return;
        };
        let width = u16::try_from(columns.len()).unwrap_or(u16::MAX);
        self.0.push((Rect::new(x, y, width, 1), spot));
    }

    pub fn first(mut self, lines: usize) -> Spots<S> {
        self.0.retain(|(area, _)| usize::from(area.y) < lines);
        self
    }

    pub fn down(self, lines: usize) -> Spots<S> {
        self.moved(0, u16::try_from(lines).unwrap_or(u16::MAX))
    }

    fn moved(self, right: u16, down: u16) -> Spots<S> {
        let moved = |(area, spot): (Rect, S)| {
            let x = area.x.saturating_add(right);
            (
                Rect {
                    x,
                    y: area.y.saturating_add(down),
                    ..area
                },
                spot,
            )
        };
        Spots(self.0.into_iter().map(moved).collect())
    }

    pub fn placed(self, area: Rect) -> Spots<S> {
        let Spots(moved) = self.moved(area.x, area.y);
        let cut = moved
            .into_iter()
            .map(|(spot_area, spot)| (spot_area.intersection(area), spot));
        Spots(cut.filter(|(area, _)| !area.is_empty()).collect())
    }

    pub fn extend(&mut self, spots: Spots<S>) {
        self.0.extend(spots.0);
    }

    pub fn map<T>(self, does: impl Fn(S) -> T) -> Spots<T> {
        Spots(
            self.0
                .into_iter()
                .map(|(area, spot)| (area, does(spot)))
                .collect(),
        )
    }

    pub fn at(&self, at: Position) -> Option<&S> {
        self.0
            .iter()
            .find(|(area, _)| area.contains(at))
            .map(|(_, spot)| spot)
    }

    pub fn hovered(&self, pointer: Position) -> Option<Rect> {
        self.0
            .iter()
            .find(|(area, _)| area.contains(pointer))
            .map(|(area, _)| *area)
    }

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

    fn raise(&self, buffer: &mut Buffer, pointer: Position) {
        let Some((area, _)) = self.0.iter().find(|(area, _)| area.contains(pointer)) else {
            return;
        };
        for at in area.positions() {
            if let Some(cell) = buffer.cell_mut(at) {
                let raised = match cell.bg {
                    Color::Reset => POINTED,
                    POINTED => moonlight::LIFTED,
                    _ => moonlight::RAISED,
                };
                cell.set_bg(raised);
            }
        }
    }
}

#[derive(Debug, Default)]
pub enum SidebarBottom {
    #[default]
    Nothing,
    Keys(Keyed, Spots<KeyEvent>),
    Menu {
        number: u64,
        spots: Spots<Choice>,
    },
}

#[derive(Debug, Default)]
pub enum Drawn {
    #[default]
    Nothing,
    Menu {
        number: u64,
        area: Rect,
        spots: Spots<Choice>,
    },
    Picker {
        number: u64,
        area: Rect,
        spots: Spots<Pick>,
    },
    Sidebar {
        numbers: Spots<Link>,
        bottom: SidebarBottom,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawnHover {
    None,
    Spot(Rect),
    Sidebar {
        number: Option<Rect>,
        bottom: Option<Rect>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyed {
    Legend,
    Search,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Keys(Keyed, KeyEvent),
    Menu(u64, Choice),
    Picker(u64, Pick),
    Number(Link),
    Aloud(Button),
}

impl SidebarBottom {
    pub fn at(&self, at: Position) -> Option<Hit> {
        match self {
            SidebarBottom::Nothing => None,
            SidebarBottom::Keys(keyed, spots) => spots.at(at).map(|key| Hit::Keys(*keyed, *key)),
            SidebarBottom::Menu { number, spots } => spots
                .at(at)
                .cloned()
                .map(|choice| Hit::Menu(*number, choice)),
        }
    }

    pub fn light(&self, buffer: &mut Buffer, pointer: Position) {
        match self {
            SidebarBottom::Nothing => {}
            SidebarBottom::Keys(_, spots) => spots.light(buffer, pointer),
            SidebarBottom::Menu { spots, .. } => spots.light(buffer, pointer),
        }
    }

    #[must_use]
    pub fn hovered(&self, pointer: Position) -> Option<Rect> {
        match self {
            SidebarBottom::Nothing => None,
            SidebarBottom::Keys(_, spots) => spots.hovered(pointer),
            SidebarBottom::Menu { spots, .. } => spots.hovered(pointer),
        }
    }

    pub fn placed(self, lines: usize, area: Rect) -> SidebarBottom {
        match self {
            SidebarBottom::Nothing => SidebarBottom::Nothing,
            SidebarBottom::Keys(keyed, spots) => {
                SidebarBottom::Keys(keyed, spots.down(lines).placed(area))
            }
            SidebarBottom::Menu { number, spots } => SidebarBottom::Menu {
                number,
                spots: spots.down(lines).placed(area),
            },
        }
    }
}

impl Drawn {
    pub fn boxed(&self) -> Option<Rect> {
        match self {
            Drawn::Menu { area, .. } | Drawn::Picker { area, .. } => Some(*area),
            Drawn::Nothing | Drawn::Sidebar { .. } => None,
        }
    }

    pub fn at(&self, at: Position) -> Option<Hit> {
        match self {
            Drawn::Nothing => None,
            Drawn::Menu { number, spots, .. } => spots
                .at(at)
                .cloned()
                .map(|choice| Hit::Menu(*number, choice)),
            Drawn::Picker { number, spots, .. } => {
                spots.at(at).cloned().map(|pick| Hit::Picker(*number, pick))
            }
            Drawn::Sidebar { numbers, bottom } => numbers
                .at(at)
                .cloned()
                .map(Hit::Number)
                .or_else(|| bottom.at(at)),
        }
    }

    pub fn light(&self, buffer: &mut Buffer, pointer: Position) {
        match self {
            Drawn::Nothing => {}
            Drawn::Menu { spots, .. } => spots.light(buffer, pointer),
            Drawn::Picker { spots, .. } => spots.light(buffer, pointer),
            Drawn::Sidebar { numbers, bottom } => {
                numbers.raise(buffer, pointer);
                bottom.light(buffer, pointer);
            }
        }
    }

    #[must_use]
    pub fn hovered(&self, pointer: Position) -> DrawnHover {
        match self {
            Drawn::Nothing => DrawnHover::None,
            Drawn::Menu { spots, .. } => spots
                .hovered(pointer)
                .map_or(DrawnHover::None, DrawnHover::Spot),
            Drawn::Picker { spots, .. } => spots
                .hovered(pointer)
                .map_or(DrawnHover::None, DrawnHover::Spot),
            Drawn::Sidebar { numbers, bottom } => {
                match (numbers.hovered(pointer), bottom.hovered(pointer)) {
                    (None, None) => DrawnHover::None,
                    (number, bottom) => DrawnHover::Sidebar { number, bottom },
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Burst {
    at: Position,
    last: Instant,
}

impl Burst {
    pub fn new(at: Position, now: Instant) -> Burst {
        Burst { at, last: now }
    }

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

#[derive(Debug, Default)]
pub struct Settling {
    areas: Vec<Rect>,
    changed: Option<(Vec<Rect>, Instant)>,
}

impl Settling {
    pub fn drawn<S>(&mut self, spots: &Spots<S>, now: Instant) {
        let areas: Vec<Rect> = spots.0.iter().map(|(area, _)| *area).collect();
        if areas != self.areas {
            let mut changed = std::mem::replace(&mut self.areas, areas);
            changed.extend_from_slice(&self.areas);
            self.changed = Some((changed, now));
        }
    }

    pub fn takes(&self, at: Position, now: Instant) -> bool {
        self.changed.as_ref().is_some_and(|(areas, since)| {
            now.saturating_duration_since(*since) < UNSEEN
                && areas.iter().any(|area| area.contains(at))
        })
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

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
        assert_eq!(placed.at(Position::new(17, 22)), Some(&key('r')));
        assert_eq!(placed.at(Position::new(18, 22)), None);
        assert_eq!(placed.at(Position::new(10, 25)), None);
        assert_eq!(placed.at(Position::new(10, 20)), None);
    }

    #[test]
    fn the_pointer_lights_the_spot_it_is_on() {
        let mut spots = Spots::default();
        spots.add(0, 0..4, key('a'));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        buffer[(1, 0)].set_bg(moonlight::LIFTED);
        spots.light(&mut buffer, Position::new(5, 0));
        spots.light(&mut buffer, Position::new(0, 1));
        assert!(buffer.content().iter().all(|cell| cell.bg != POINTED));
        spots.light(&mut buffer, Position::new(2, 0));
        let lit: Vec<Color> = (0..6).map(|x| buffer[(x, 0)].bg).collect();
        assert_eq!(
            lit,
            [
                POINTED,
                moonlight::LIFTED,
                POINTED,
                POINTED,
                Color::Reset,
                Color::Reset
            ]
        );
    }

    #[test]
    fn the_pointer_raises_a_number_whatever_its_row_is_on() {
        let mut spots = Spots::default();
        spots.add(
            0,
            1..3,
            Link::parse("https://github.com/o/r/pull/1").expect("a link"),
        );
        for (on, raised) in [
            (Color::Reset, POINTED),
            (POINTED, moonlight::LIFTED),
            (moonlight::LIFTED, moonlight::RAISED),
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
            buffer.set_style(buffer.area, ratatui::style::Style::new().bg(on));
            spots.raise(&mut buffer, Position::new(3, 0));
            assert!(buffer.content().iter().all(|cell| cell.bg == on));
            spots.raise(&mut buffer, Position::new(2, 0));
            let lit: Vec<Color> = (0..4).map(|x| buffer[(x, 0)].bg).collect();
            assert_eq!(lit, [on, raised, raised, on]);
        }
    }

    #[test]
    fn a_click_hits_a_spot_with_what_drew_it() {
        let mut spots = Spots::default();
        spots.add(0, 0..4, key('d'));
        let area = Rect::new(0, 0, 4, 1);
        let at = Position::new(1, 0);
        let legend = Drawn::Sidebar {
            numbers: Spots::default(),
            bottom: SidebarBottom::Keys(Keyed::Legend, spots.clone()),
        };
        assert_eq!(legend.at(at), Some(Hit::Keys(Keyed::Legend, key('d'))));
        assert_eq!(legend.boxed(), None);
        let docked = Drawn::Sidebar {
            numbers: Spots::default(),
            bottom: SidebarBottom::Menu {
                number: 3,
                spots: spots.clone().map(Choice::Key),
            },
        };
        assert_eq!(docked.at(at), Some(Hit::Menu(3, Choice::Key(key('d')))));
        assert_eq!(docked.boxed(), None);
        let floating = Drawn::Menu {
            number: 3,
            area,
            spots: spots.clone().map(Choice::Key),
        };
        assert_eq!(floating.at(at), Some(Hit::Menu(3, Choice::Key(key('d')))));
        assert_eq!(floating.boxed(), Some(area));
        assert_eq!(Drawn::Nothing.at(at), None);
    }

    #[test]
    fn every_fast_click_on_the_cell_is_one_more_of_the_burst() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let at = Position::new(4, 14);
        let mut burst = Burst::new(at, start);
        assert!(burst.takes(at, after(150)));
        assert!(burst.takes(at, after(300)));
        assert!(burst.takes(at, after(750)));
        assert!(!burst.takes(at, after(1300)));
        for off in [
            Position::new(6, 14),
            Position::new(2, 14),
            Position::new(4, 16),
        ] {
            let mut burst = Burst::new(at, start);
            assert!(!burst.takes(off, after(100)), "{off:?}");
        }
    }

    #[test]
    fn a_fast_click_next_to_the_last_is_one_more_of_the_burst() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let mut burst = Burst::new(Position::new(4, 14), start);
        assert!(burst.takes(Position::new(5, 14), after(150)));
        assert!(burst.takes(Position::new(5, 15), after(300)));
        assert!(burst.takes(Position::new(6, 14), after(450)));
    }

    fn at_line(line: usize, spot: char) -> Spots<char> {
        let mut spots = Spots::default();
        spots.add(line, 10..13, spot);
        spots
    }

    #[test]
    fn a_click_on_where_spots_just_went_is_on_nothing() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let on = Position::new(11, 5);
        let mut settling = Settling::default();
        settling.drawn(&at_line(5, 'a'), start);
        settling.drawn(&Spots::<char>::default(), after(1000));
        assert!(settling.takes(on, after(1100)));
        assert!(!settling.takes(on, after(1400)), "you have seen them go");
        assert!(
            !settling.takes(Position::new(9, 5), after(1100)),
            "beside them"
        );
        assert!(
            !settling.takes(Position::new(11, 4), after(1100)),
            "above them"
        );
    }

    #[test]
    fn a_click_on_where_spots_just_came_is_on_nothing() {
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        let on = Position::new(11, 5);
        let mut settling = Settling::default();
        settling.drawn(&Spots::<char>::default(), start);
        assert!(!settling.takes(on, start), "nothing has changed");
        settling.drawn(&at_line(5, 'a'), after(1000));
        assert!(settling.takes(on, after(1100)));
        assert!(!settling.takes(on, after(1400)), "you have seen them come");
    }

    #[test]
    fn a_click_on_where_spots_just_moved_from_or_to_is_on_nothing() {
        let start = Instant::now();
        let mut settling = Settling::default();
        settling.drawn(&at_line(5, 'a'), start);
        settling.drawn(&at_line(3, 'a'), start + Duration::from_secs(1));
        let soon = start + Duration::from_millis(1100);
        assert!(settling.takes(Position::new(11, 5), soon));
        assert!(settling.takes(Position::new(11, 3), soon));
    }

    #[test]
    fn spots_that_stay_where_they_are_take_a_click() {
        let start = Instant::now();
        let mut settling = Settling::default();
        settling.drawn(&at_line(5, 'a'), start);
        let later = start + Duration::from_secs(1);
        settling.drawn(&at_line(5, 'a'), later);
        settling.drawn(&at_line(5, 'b'), later);
        assert!(!settling.takes(Position::new(11, 5), later));
    }

    #[test]
    fn hovered_reports_sidebar_numbers_and_bottom_spots_without_recursion() {
        let mut numbers = Spots::default();
        numbers.add(
            1,
            2..6,
            Link::parse("https://github.com/o/r/pull/1").unwrap(),
        );
        let mut keys = Spots::default();
        keys.add(
            4,
            1..5,
            KeyEvent::from(crossterm::event::KeyCode::Char('g')),
        );
        let drawn = Drawn::Sidebar {
            numbers,
            bottom: SidebarBottom::Keys(Keyed::Legend, keys),
        };
        assert_eq!(
            drawn.hovered(Position::new(3, 1)),
            DrawnHover::Sidebar {
                number: Some(Rect::new(2, 1, 4, 1)),
                bottom: None
            }
        );
        assert_eq!(
            drawn.hovered(Position::new(2, 4)),
            DrawnHover::Sidebar {
                number: None,
                bottom: Some(Rect::new(1, 4, 4, 1))
            }
        );
        assert_eq!(drawn.hovered(Position::new(0, 0)), DrawnHover::None);
    }
}
