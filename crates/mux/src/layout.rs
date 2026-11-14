use std::cmp::Reverse;
use std::num::NonZeroU16;

use crate::asked::Size;
use crate::id::PaneId;

pub const FRAME: u16 = 1;

pub const COLUMNS_MIN: u16 = 5;

pub const ROWS_MIN: u16 = 3;

const SHARES: u32 = 1_000_000;

const STEP_PERCENT: u32 = 5;

const _: () = assert!(COLUMNS_MIN > 2 * FRAME && ROWS_MIN > 2 * FRAME);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Towards {
    Right,
    Down,
}

impl Towards {
    #[must_use]
    pub fn across(self) -> Towards {
        match self {
            Towards::Right => Towards::Down,
            Towards::Down => Towards::Right,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub const ALL: [Direction; 4] =
        [Direction::Left, Direction::Right, Direction::Up, Direction::Down];

    fn border(self) -> (Towards, Side) {
        match self {
            Direction::Left => (Towards::Right, Side::Second),
            Direction::Right => (Towards::Right, Side::First),
            Direction::Up => (Towards::Down, Side::Second),
            Direction::Down => (Towards::Down, Side::First),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Grow,
    Shrink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    First,
    Second,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub column: u16,
    pub row: u16,
    pub columns: u16,
    pub rows: u16,
}

impl Rect {
    #[must_use]
    pub fn whole(size: Size) -> Rect {
        let (columns, rows) = size.get();
        Rect { column: 0, row: 0, columns, rows }
    }

    #[must_use]
    pub fn inner(self) -> Size {
        let side = |side: u16| {
            NonZeroU16::new(side.saturating_sub(2 * FRAME)).unwrap_or(NonZeroU16::MIN)
        };
        Size { columns: side(self.columns), rows: side(self.rows) }
    }

    fn right(self) -> u16 {
        self.column + self.columns
    }

    fn bottom(self) -> u16 {
        self.row + self.rows
    }

    fn extent(self, towards: Towards) -> u16 {
        match towards {
            Towards::Right => self.columns,
            Towards::Down => self.rows,
        }
    }

    fn cut(self, towards: Towards, first: u16) -> (Rect, Rect) {
        assert!(first <= self.extent(towards), "{first} cells cut from {self:?}");
        match towards {
            Towards::Right => (
                Rect { columns: first, ..self },
                Rect { column: self.column + first, columns: self.columns - first, ..self },
            ),
            Towards::Down => (
                Rect { rows: first, ..self },
                Rect { row: self.row + first, rows: self.rows - first, ..self },
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree {
    Pane(PaneId),
    Split(Box<Split>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    towards: Towards,
    share: u32,
    first: Tree,
    second: Tree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    Absent,
    Last,
    Out { heir: PaneId },
}

enum Bordered {
    Absent,
    NoBorder,
    Border { moved: bool },
}

impl Tree {
    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.gather(&mut panes);
        panes
    }

    fn gather(&self, panes: &mut Vec<PaneId>) {
        match self {
            Tree::Pane(pane) => panes.push(*pane),
            Tree::Split(split) => {
                split.first.gather(panes);
                split.second.gather(panes);
            },
        }
    }

    #[must_use]
    pub fn holds(&self, pane: PaneId) -> bool {
        match self {
            Tree::Pane(held) => *held == pane,
            Tree::Split(split) => split.first.holds(pane) || split.second.holds(pane),
        }
    }

    #[must_use]
    pub fn fits(&self, size: Size) -> bool {
        let (columns, rows) = size.get();
        self.least(Towards::Right) <= columns && self.least(Towards::Down) <= rows
    }

    fn least(&self, towards: Towards) -> u16 {
        match self {
            Tree::Pane(_) => match towards {
                Towards::Right => COLUMNS_MIN,
                Towards::Down => ROWS_MIN,
            },
            Tree::Split(split) if split.towards == towards => {
                split.first.least(towards).saturating_add(split.second.least(towards))
            },
            Tree::Split(split) => split.first.least(towards).max(split.second.least(towards)),
        }
    }

    #[must_use]
    pub fn rects(&self, size: Size) -> Vec<(PaneId, Rect)> {
        let mut rects = Vec::new();
        self.laid(Rect::whole(size), &mut rects);
        rects
    }

    fn laid(&self, rect: Rect, rects: &mut Vec<(PaneId, Rect)>) {
        match self {
            Tree::Pane(pane) => rects.push((*pane, rect)),
            Tree::Split(split) => {
                let (first, second) = split.halves(rect);
                split.first.laid(first, rects);
                split.second.laid(second, rects);
            },
        }
    }

    pub fn split(&mut self, at: PaneId, towards: Towards, new: PaneId) -> bool {
        assert!(!self.holds(new), "pane {new} is split into a tree that holds it");
        let Some(leaf) = self.leaf_mut(at) else {
            return false;
        };
        *leaf = Tree::Split(Box::new(Split {
            towards,
            share: SHARES / 2,
            first: Tree::Pane(at),
            second: Tree::Pane(new),
        }));
        true
    }

    fn leaf_mut(&mut self, pane: PaneId) -> Option<&mut Tree> {
        match self {
            Tree::Pane(held) if *held == pane => Some(self),
            Tree::Pane(_) => None,
            Tree::Split(split) => {
                if split.first.holds(pane) {
                    split.first.leaf_mut(pane)
                } else {
                    split.second.leaf_mut(pane)
                }
            },
        }
    }

    #[must_use]
    pub fn largest(&self, size: Size) -> (PaneId, Towards) {
        let rects = self.rects(size);
        let area = |rect: &Rect| u32::from(rect.columns) * u32::from(rect.rows);
        let (pane, rect) = rects
            .iter()
            .enumerate()
            .max_by_key(|(place, (_, rect))| (area(rect), Reverse(*place)))
            .map(|(_, found)| *found)
            .expect("a tree holds a pane");
        let towards = if rect.columns > rect.rows.saturating_mul(2) {
            Towards::Right
        } else {
            Towards::Down
        };
        (pane, towards)
    }

    pub fn remove(&mut self, pane: PaneId) -> Removed {
        if *self == Tree::Pane(pane) {
            return Removed::Last;
        }
        match self.removed(pane) {
            Some(heir) => Removed::Out { heir },
            None => Removed::Absent,
        }
    }

    fn removed(&mut self, pane: PaneId) -> Option<PaneId> {
        let Tree::Split(split) = self else {
            return None;
        };
        let side = if split.first == Tree::Pane(pane) {
            Side::First
        } else if split.second == Tree::Pane(pane) {
            Side::Second
        } else {
            return split.first.removed(pane).or_else(|| split.second.removed(pane));
        };
        let Tree::Split(split) = std::mem::replace(self, Tree::Pane(pane)) else {
            unreachable!("the tree was a split a moment ago");
        };
        let Split { first, second, .. } = *split;
        *self = match side {
            Side::First => second,
            Side::Second => first,
        };
        let panes = self.panes();
        let heir = match side {
            Side::First => panes.first(),
            Side::Second => panes.last(),
        };
        Some(*heir.expect("a tree holds a pane"))
    }

    pub fn swap(&mut self, one: PaneId, other: PaneId) {
        assert!(self.holds(one) && self.holds(other), "panes {one} and {other} are swapped");
        self.swapped(one, other);
    }

    fn swapped(&mut self, one: PaneId, other: PaneId) {
        match self {
            Tree::Pane(pane) if *pane == one => *pane = other,
            Tree::Pane(pane) if *pane == other => *pane = one,
            Tree::Pane(_) => {},
            Tree::Split(split) => {
                split.first.swapped(one, other);
                split.second.swapped(one, other);
            },
        }
    }

    #[must_use]
    pub fn neighbour(&self, size: Size, pane: PaneId, direction: Direction) -> Option<PaneId> {
        let rects = self.rects(size);
        let (_, from) = *rects.iter().find(|(found, _)| *found == pane)?;
        rects
            .iter()
            .enumerate()
            .filter(|(_, (other, _))| *other != pane)
            .filter_map(|(place, (other, rect))| {
                Some((touching(from, *rect, direction)?, Reverse(place), *other))
            })
            .max_by_key(|(overlap, place, _)| (*overlap, *place))
            .map(|(_, _, other)| other)
    }

    #[must_use]
    pub fn beside(&self, pane: PaneId, forward: bool) -> Option<PaneId> {
        let panes = self.panes();
        let place = panes.iter().position(|found| *found == pane)?;
        if panes.len() == 1 {
            return None;
        }
        let next = if forward {
            (place + 1) % panes.len()
        } else {
            (place + panes.len() - 1) % panes.len()
        };
        Some(panes[next])
    }

    pub fn resize(
        &mut self,
        size: Size,
        pane: PaneId,
        direction: Direction,
        change: Change,
    ) -> bool {
        let (towards, edge) = direction.border();
        matches!(
            self.resized(Rect::whole(size), pane, towards, edge, change),
            Bordered::Border { moved: true }
        )
    }

    fn resized(
        &mut self,
        rect: Rect,
        pane: PaneId,
        towards: Towards,
        side: Side,
        change: Change,
    ) -> Bordered {
        let Tree::Split(split) = self else {
            return if *self == Tree::Pane(pane) { Bordered::NoBorder } else { Bordered::Absent };
        };
        let (first, second) = split.halves(rect);
        let (held, deeper) = if split.first.holds(pane) {
            (Side::First, split.first.resized(first, pane, towards, side, change))
        } else {
            (Side::Second, split.second.resized(second, pane, towards, side, change))
        };
        match deeper {
            Bordered::NoBorder if split.towards == towards && held == side => {
                Bordered::Border { moved: split.moved(rect.extent(towards), side, change) }
            },
            deeper => deeper,
        }
    }
}

impl Split {
    fn first_extent(&self, extent: u16) -> u16 {
        let wanted = (u64::from(extent) * u64::from(self.share) + u64::from(SHARES / 2))
            / u64::from(SHARES);
        let wanted = u16::try_from(wanted).expect("a share of a side is no more than the side");
        let (low, high) = self.bounds(extent);
        if low <= high { wanted.clamp(low, high) } else { wanted }
    }

    fn bounds(&self, extent: u16) -> (u16, u16) {
        let low = self.first.least(self.towards);
        let high = extent.saturating_sub(self.second.least(self.towards));
        (low, high)
    }

    fn halves(&self, rect: Rect) -> (Rect, Rect) {
        rect.cut(self.towards, self.first_extent(rect.extent(self.towards)))
    }

    fn moved(&mut self, extent: u16, side: Side, change: Change) -> bool {
        let (low, high) = self.bounds(extent);
        if low > high || extent == 0 {
            return false;
        }
        let now = self.first_extent(extent);
        let step = u16::try_from(u32::from(extent) * STEP_PERCENT / 100)
            .expect("a step is less than the side")
            .max(1);
        let grows_first = matches!(
            (side, change),
            (Side::First, Change::Grow) | (Side::Second, Change::Shrink)
        );
        let wanted = if grows_first { now.saturating_add(step) } else { now.saturating_sub(step) };
        let wanted = wanted.clamp(low, high);
        if wanted == now {
            return false;
        }
        let share = (u64::from(wanted) * u64::from(SHARES) + u64::from(extent / 2))
            / u64::from(extent);
        self.share = u32::try_from(share).expect("a share is at most all of them");
        assert_eq!(self.first_extent(extent), wanted, "a moved border lands where it was moved");
        true
    }
}

fn touching(from: Rect, to: Rect, direction: Direction) -> Option<u16> {
    let overlap = |start: u16, end: u16, other_start: u16, other_end: u16| {
        end.min(other_end).saturating_sub(start.max(other_start))
    };
    let (adjacent, overlap) = match direction {
        Direction::Left => {
            (to.right() == from.column, overlap(from.row, from.bottom(), to.row, to.bottom()))
        },
        Direction::Right => {
            (from.right() == to.column, overlap(from.row, from.bottom(), to.row, to.bottom()))
        },
        Direction::Up => (
            to.bottom() == from.row,
            overlap(from.column, from.right(), to.column, to.right()),
        ),
        Direction::Down => (
            from.bottom() == to.row,
            overlap(from.column, from.right(), to.column, to.right()),
        ),
    };
    (adjacent && overlap > 0).then_some(overlap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32) -> PaneId {
        PaneId::parse("pane", id).unwrap()
    }

    fn size(columns: u16, rows: u16) -> Size {
        Size { columns: NonZeroU16::new(columns).unwrap(), rows: NonZeroU16::new(rows).unwrap() }
    }

    fn rect(column: u16, row: u16, columns: u16, rows: u16) -> Rect {
        Rect { column, row, columns, rows }
    }

    fn three() -> Tree {
        let mut tree = Tree::Pane(pane(1));
        assert!(tree.split(pane(1), Towards::Right, pane(2)));
        assert!(tree.split(pane(2), Towards::Down, pane(3)));
        tree
    }

    fn tiles(tree: &Tree, size: Size) {
        let rects = tree.rects(size);
        let (columns, rows) = size.get();
        let mut covered = vec![0u8; usize::from(columns) * usize::from(rows)];
        for (_, rect) in &rects {
            for row in rect.row..rect.bottom() {
                for column in rect.column..rect.right() {
                    covered[usize::from(row) * usize::from(columns) + usize::from(column)] += 1;
                }
            }
        }
        assert!(covered.iter().all(|count| *count == 1), "{rects:?} tile {size:?}");
    }

    #[test]
    fn a_lone_pane_takes_the_tab_and_its_frame_takes_a_cell_each_side() {
        let tree = Tree::Pane(pane(1));
        assert_eq!(tree.rects(size(80, 24)), [(pane(1), rect(0, 0, 80, 24))]);
        assert_eq!(rect(0, 0, 80, 24).inner(), size(78, 22));
        assert_eq!(rect(0, 0, 2, 1).inner(), size(1, 1));
    }

    #[test]
    fn splits_halve_the_pane_split_and_tile_the_tab() {
        let tree = three();
        assert_eq!(
            tree.rects(size(80, 24)),
            [
                (pane(1), rect(0, 0, 40, 24)),
                (pane(2), rect(40, 0, 40, 12)),
                (pane(3), rect(40, 12, 40, 12)),
            ]
        );
        assert_eq!(tree.panes(), [pane(1), pane(2), pane(3)]);
        for (columns, rows) in [(80, 24), (81, 25), (11, 7), (200, 60)] {
            tiles(&tree, size(columns, rows));
        }
    }

    #[test]
    fn a_tab_too_small_for_its_panes_does_not_fit_them() {
        let tree = three();
        assert!(tree.fits(size(10, 6)));
        assert!(!tree.fits(size(9, 6)));
        assert!(!tree.fits(size(10, 5)));
        tiles(&tree, size(4, 2));
    }

    #[test]
    fn the_largest_pane_is_split_along_its_longer_side() {
        let tree = three();
        assert_eq!(tree.largest(size(80, 24)), (pane(1), Towards::Down));
        assert_eq!(tree.largest(size(200, 24)), (pane(1), Towards::Right));
        assert_eq!(Tree::Pane(pane(4)).largest(size(80, 24)), (pane(4), Towards::Right));
        let mut even = Tree::Pane(pane(1));
        even.split(pane(1), Towards::Right, pane(2));
        assert_eq!(even.largest(size(80, 24)).0, pane(1));
    }

    #[test]
    fn a_removed_pane_leaves_its_room_to_its_sibling() {
        let mut tree = three();
        assert_eq!(tree.remove(pane(2)), Removed::Out { heir: pane(3) });
        assert_eq!(tree.rects(size(80, 24))[1], (pane(3), rect(40, 0, 40, 24)));
        assert_eq!(tree.remove(pane(9)), Removed::Absent);
        assert_eq!(tree.remove(pane(3)), Removed::Out { heir: pane(1) });
        assert_eq!(tree, Tree::Pane(pane(1)));
        assert_eq!(tree.remove(pane(1)), Removed::Last);
        let mut tree = three();
        assert_eq!(tree.remove(pane(1)), Removed::Out { heir: pane(2) });
        assert_eq!(tree.panes(), [pane(2), pane(3)]);
    }

    #[test]
    fn a_neighbour_is_the_pane_that_shares_the_most_of_that_edge() {
        let tree = three();
        let at = size(80, 24);
        assert_eq!(tree.neighbour(at, pane(1), Direction::Right), Some(pane(2)));
        assert_eq!(tree.neighbour(at, pane(3), Direction::Left), Some(pane(1)));
        assert_eq!(tree.neighbour(at, pane(2), Direction::Down), Some(pane(3)));
        assert_eq!(tree.neighbour(at, pane(3), Direction::Up), Some(pane(2)));
        assert_eq!(tree.neighbour(at, pane(1), Direction::Left), None);
        assert_eq!(tree.neighbour(at, pane(1), Direction::Up), None);
        assert_eq!(tree.neighbour(at, pane(9), Direction::Up), None);
        assert_eq!(tree.beside(pane(3), true), Some(pane(1)));
        assert_eq!(tree.beside(pane(1), false), Some(pane(3)));
        assert_eq!(Tree::Pane(pane(1)).beside(pane(1), true), None);
    }

    #[test]
    fn a_resize_moves_the_nearest_border_on_that_side() {
        let mut tree = three();
        let at = size(80, 24);
        assert!(tree.resize(at, pane(3), Direction::Left, Change::Grow));
        assert_eq!(tree.rects(at)[0].1, rect(0, 0, 36, 24));
        assert!(tree.resize(at, pane(3), Direction::Up, Change::Grow));
        assert_eq!(tree.rects(at)[2].1, rect(36, 11, 44, 13));
        assert!(tree.resize(at, pane(1), Direction::Right, Change::Shrink));
        assert_eq!(tree.rects(at)[0].1, rect(0, 0, 32, 24));
        assert!(!tree.resize(at, pane(1), Direction::Left, Change::Grow));
        assert!(!tree.resize(at, pane(2), Direction::Up, Change::Grow));
        tiles(&tree, at);
    }

    #[test]
    fn a_border_stops_where_a_pane_would_be_too_small() {
        let mut tree = Tree::Pane(pane(1));
        tree.split(pane(1), Towards::Right, pane(2));
        let at = size(20, 5);
        while tree.resize(at, pane(1), Direction::Right, Change::Grow) {}
        assert_eq!(tree.rects(at)[1].1, rect(15, 0, COLUMNS_MIN, 5));
        assert!(!tree.resize(at, pane(1), Direction::Right, Change::Grow));
        let cramped = size(8, 5);
        assert!(!tree.resize(cramped, pane(2), Direction::Left, Change::Grow));
    }

    #[test]
    fn a_swap_trades_two_panes_places() {
        let mut tree = three();
        tree.swap(pane(1), pane(3));
        assert_eq!(tree.panes(), [pane(3), pane(2), pane(1)]);
        let mut lone = Tree::Pane(pane(1));
        lone.swap(pane(1), pane(1));
        assert_eq!(lone, Tree::Pane(pane(1)));
    }
}
