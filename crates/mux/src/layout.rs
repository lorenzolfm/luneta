use std::cmp::Reverse;
use std::num::NonZeroU16;

use crate::asked::Size;
use crate::id::PaneId;
use crate::mux::PANES_MAX;

pub const FRAME_CELLS: u16 = 1;

pub const COLUMNS_MIN: u16 = 5;

pub const ROWS_MIN: u16 = 3;

const SHARES: u32 = 1_000_000;

const STEP_PERCENT: u32 = 5;

const NODES_MAX: usize = 2 * PANES_MAX - 1;

const ROOT: usize = 0;

const _: () = assert!(COLUMNS_MIN > 2 * FRAME_CELLS);
const _: () = assert!(ROWS_MIN > 2 * FRAME_CELLS);
const _: () = assert!(PANES_MAX * COLUMNS_MIN as usize <= u16::MAX as usize);
const _: () = assert!(PANES_MAX * ROWS_MIN as usize <= u16::MAX as usize);

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
pub enum Order {
    Next,
    Previous,
}

impl Order {
    #[must_use]
    pub fn stepped(self, place: usize, count: usize) -> usize {
        assert!(place < count, "place {place} is one of {count}");
        match self {
            Order::Next => (place + 1) % count,
            Order::Previous => (place + count - 1) % count,
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
        assert!(self.columns >= COLUMNS_MIN, "{self:?} is narrower than a pane");
        assert!(self.rows >= ROWS_MIN, "{self:?} is shorter than a pane");
        let side = |side: u16| {
            NonZeroU16::new(side - 2 * FRAME_CELLS).expect("a pane holds its frame and a cell")
        };
        Size { columns: side(self.columns), rows: side(self.rows) }
    }

    fn right(self) -> u16 {
        self.column + self.columns
    }

    fn bottom(self) -> u16 {
        self.row + self.rows
    }

    fn area(self) -> u64 {
        u64::from(self.columns) * u64::from(self.rows)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Least {
    columns: u16,
    rows: u16,
}

impl Least {
    const PANE: Least = Least { columns: COLUMNS_MIN, rows: ROWS_MIN };

    fn along(self, towards: Towards) -> u16 {
        match towards {
            Towards::Right => self.columns,
            Towards::Down => self.rows,
        }
    }

    fn joined(self, other: Least, towards: Towards) -> Least {
        match towards {
            Towards::Right => Least {
                columns: self.columns + other.columns,
                rows: self.rows.max(other.rows),
            },
            Towards::Down => Least {
                columns: self.columns.max(other.columns),
                rows: self.rows + other.rows,
            },
        }
    }

    fn fits(self, size: Size) -> bool {
        let (columns, rows) = size.get();
        self.columns <= columns && self.rows <= rows
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Node {
    Pane(PaneId),
    Split(Split),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Split {
    towards: Towards,
    share: u32,
    first: usize,
    second: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    Last,
    Out { heir: PaneId },
}

impl Tree {
    #[must_use]
    pub fn one(pane: PaneId) -> Tree {
        Tree { nodes: vec![Node::Pane(pane)] }
    }

    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        self.order().into_iter().map(|node| self.pane_of(node)).collect()
    }

    #[must_use]
    pub fn holds(&self, pane: PaneId) -> bool {
        self.leaf(pane).is_some()
    }

    #[must_use]
    pub fn fits(&self, size: Size) -> bool {
        self.leasts()[ROOT].fits(size)
    }

    #[must_use]
    pub fn fitted(&self, size: Size) -> Size {
        let least = self.leasts()[ROOT];
        let raised = |cells: NonZeroU16, least: u16| {
            cells.max(NonZeroU16::new(least).expect("a pane is a cell or more each way"))
        };
        let fitted = Size {
            columns: raised(size.columns, least.columns),
            rows: raised(size.rows, least.rows),
        };
        assert!(least.fits(fitted), "{fitted:?} holds the panes of the tree");
        fitted
    }

    #[must_use]
    pub fn rects(&self, size: Size) -> Vec<(PaneId, Rect)> {
        let rects = self.node_rects(size, &self.leasts());
        let laid: Vec<(PaneId, Rect)> =
            self.order().into_iter().map(|node| (self.pane_of(node), rects[node])).collect();
        assert_tiled(&laid, size);
        laid
    }

    pub fn split(&mut self, at: PaneId, towards: Towards, new: PaneId) {
        assert!(!self.holds(new), "pane {new} is split into a tree that holds it");
        assert!(self.nodes.len() + 2 <= NODES_MAX, "a tree holds at most {PANES_MAX} panes");
        let node = self.leaf(at).expect("a split pane is in the tree");
        let first = self.nodes.len();
        self.nodes.push(Node::Pane(at));
        self.nodes.push(Node::Pane(new));
        self.nodes[node] =
            Node::Split(Split { towards, share: SHARES / 2, first, second: first + 1 });
        self.assert_shaped();
    }

    #[must_use]
    pub fn largest(&self, size: Size) -> (PaneId, Towards) {
        let rects = self.rects(size);
        let (pane, rect) = rects
            .iter()
            .enumerate()
            .max_by_key(|(place, (_, rect))| (rect.area(), Reverse(*place)))
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
        let node = self.leaf(pane).expect("a removed pane is in the tree");
        if node == ROOT {
            assert_eq!(self.nodes.len(), 1, "the root is a pane only in a tree of one");
            return Removed::Last;
        }
        let panes = self.panes();
        let place = panes.iter().position(|found| *found == pane);
        let place = place.expect("a removed pane is in the tree's order");
        let parent = self.parents()[node].expect("a pane under the root has a parent");
        let Node::Split(split) = self.nodes[parent] else {
            unreachable!("node {parent} is a parent, so a split");
        };
        let (sibling, heir) = if split.first == node {
            (split.second, panes[place + 1])
        } else {
            assert_eq!(split.second, node, "a pane is one half of its parent");
            (split.first, panes[place - 1])
        };
        self.nodes[parent] = self.nodes[sibling];
        self.compact([node, sibling]);
        self.assert_shaped();
        assert!(!self.holds(pane), "pane {pane} left the tree");
        assert!(self.holds(heir), "pane {heir} took its sibling's room");
        Removed::Out { heir }
    }

    pub fn swap(&mut self, one: PaneId, other: PaneId) {
        let one_node = self.leaf(one).expect("a swapped pane is in the tree");
        let other_node = self.leaf(other).expect("a swapped pane is in the tree");
        self.nodes[one_node] = Node::Pane(other);
        self.nodes[other_node] = Node::Pane(one);
    }

    #[must_use]
    pub fn neighbour(&self, size: Size, pane: PaneId, direction: Direction) -> Option<PaneId> {
        let rects = self.rects(size);
        let from = rects.iter().find(|(found, _)| *found == pane);
        let (_, from) = *from.expect("a pane of the tree is laid out");
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
    pub fn in_order(&self, pane: PaneId, order: Order) -> Option<PaneId> {
        let panes = self.panes();
        let place = panes.iter().position(|found| *found == pane);
        let place = place.expect("a pane of the tree is in its order");
        (panes.len() > 1).then(|| panes[order.stepped(place, panes.len())])
    }

    pub fn resize(
        &mut self,
        size: Size,
        pane: PaneId,
        direction: Direction,
        change: Change,
    ) -> bool {
        let (towards, half) = direction.border();
        let leasts = self.leasts();
        let rects = self.node_rects(size, &leasts);
        let parents = self.parents();
        let mut node = self.leaf(pane).expect("a resized pane is in the tree");
        for _ in 0..self.nodes.len() {
            let Some(parent) = parents[node] else {
                return false;
            };
            let Node::Split(split) = &mut self.nodes[parent] else {
                unreachable!("node {parent} is a parent, so a split");
            };
            let held = if split.first == node { Side::First } else { Side::Second };
            if (split.towards, held) == (towards, half) {
                let extent = rects[parent].extent(towards);
                let bounds = split.bounds(extent, leasts[split.first], leasts[split.second]);
                return split.moved(extent, bounds, half, change);
            }
            node = parent;
        }
        unreachable!("a walk up a tree of {} nodes reaches its root", self.nodes.len());
    }

    fn leaf(&self, pane: PaneId) -> Option<usize> {
        self.nodes.iter().position(|node| *node == Node::Pane(pane))
    }

    fn pane_of(&self, node: usize) -> PaneId {
        match self.nodes[node] {
            Node::Pane(pane) => pane,
            Node::Split(_) => unreachable!("node {node} is laid out as a pane"),
        }
    }

    fn order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.nodes.len().div_ceil(2));
        let mut stack = Vec::with_capacity(self.nodes.len());
        stack.push(ROOT);
        for _ in 0..self.nodes.len() {
            let node = stack.pop().expect("a tree visits each of its nodes once");
            match self.nodes[node] {
                Node::Pane(_) => order.push(node),
                Node::Split(split) => {
                    stack.push(split.second);
                    stack.push(split.first);
                },
            }
        }
        assert!(stack.is_empty(), "a tree visits each of its nodes once");
        assert_eq!(order.len(), self.nodes.len().div_ceil(2), "a tree of splits in two");
        order
    }

    fn parents(&self) -> Vec<Option<usize>> {
        let mut parents = vec![None; self.nodes.len()];
        for (node, found) in self.nodes.iter().enumerate() {
            if let Node::Split(split) = found {
                for half in [split.first, split.second] {
                    assert!(half > node, "node {half} comes after its parent {node}");
                    assert!(parents[half].is_none(), "node {half} has one parent");
                    parents[half] = Some(node);
                }
            }
        }
        parents
    }

    fn leasts(&self) -> Vec<Least> {
        let mut leasts = vec![Least::PANE; self.nodes.len()];
        for (node, found) in self.nodes.iter().enumerate().rev() {
            if let Node::Split(split) = found {
                assert!(split.first > node, "node {} comes after its parent {node}", split.first);
                assert!(split.second > node, "node {} comes after its parent {node}", split.second);
                leasts[node] = leasts[split.first].joined(leasts[split.second], split.towards);
            }
        }
        leasts
    }

    fn node_rects(&self, size: Size, leasts: &[Least]) -> Vec<Rect> {
        assert_eq!(leasts.len(), self.nodes.len(), "a least size for every node");
        assert!(leasts[ROOT].fits(size), "a tab of {size:?} holds its panes");
        let mut rects = vec![Rect::whole(size); self.nodes.len()];
        for (node, found) in self.nodes.iter().enumerate() {
            if let Node::Split(split) = found {
                let (first, second) = (leasts[split.first], leasts[split.second]);
                let (first, second) = split.halves(rects[node], first, second);
                rects[split.first] = first;
                rects[split.second] = second;
            }
        }
        rects
    }

    fn compact(&mut self, gone: [usize; 2]) {
        let shifted = |node: usize| {
            assert!(!gone.contains(&node), "node {node} is gone");
            node - gone.iter().filter(|gone| **gone < node).count()
        };
        let kept: Vec<Node> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(node, _)| !gone.contains(node))
            .map(|(_, found)| match *found {
                pane @ Node::Pane(_) => pane,
                Node::Split(split) => Node::Split(Split {
                    first: shifted(split.first),
                    second: shifted(split.second),
                    ..split
                }),
            })
            .collect();
        assert_eq!(kept.len() + gone.len(), self.nodes.len(), "two nodes left the tree");
        self.nodes = kept;
    }

    fn assert_shaped(&self) {
        assert!(self.nodes.len() <= NODES_MAX, "a tree of {} nodes", self.nodes.len());
        assert_eq!(self.nodes.len() % 2, 1, "a tree of splits in two has an odd count of nodes");
        let parents = self.parents();
        assert!(parents[ROOT].is_none(), "the root has no parent");
        let hung = parents.iter().skip(1).all(Option::is_some);
        assert!(hung, "every node but the root hangs from a split");
        let mut panes = self.panes();
        let count = panes.len();
        panes.sort_unstable();
        panes.dedup();
        assert_eq!(panes.len(), count, "a tree holds each pane once");
    }
}

impl Split {
    fn bounds(self, extent: u16, first: Least, second: Least) -> (u16, u16) {
        let low = first.along(self.towards);
        let high = extent.checked_sub(second.along(self.towards));
        let high = high.expect("a split's side holds its second half");
        assert!(low <= high, "a side of {extent} cells holds a first half of {low}");
        (low, high)
    }

    fn first_extent(self, extent: u16, (low, high): (u16, u16)) -> u16 {
        assert!(self.share <= SHARES, "a share of {} is more than the whole", self.share);
        let wanted = (u64::from(extent) * u64::from(self.share) + u64::from(SHARES / 2))
            / u64::from(SHARES);
        let wanted = u16::try_from(wanted).expect("a share of a side is no more than the side");
        wanted.clamp(low, high)
    }

    fn halves(self, rect: Rect, first: Least, second: Least) -> (Rect, Rect) {
        let extent = rect.extent(self.towards);
        let bounds = self.bounds(extent, first, second);
        rect.cut(self.towards, self.first_extent(extent, bounds))
    }

    fn moved(&mut self, extent: u16, bounds: (u16, u16), side: Side, change: Change) -> bool {
        assert!(extent > 0, "a border moves on a side of some cells");
        let (low, high) = bounds;
        let now = self.first_extent(extent, bounds);
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
        let landed = self.first_extent(extent, bounds);
        assert_eq!(landed, wanted, "a moved border lands where it was moved");
        true
    }
}

fn assert_tiled(laid: &[(PaneId, Rect)], size: Size) {
    let (columns, rows) = size.get();
    let mut area = 0;
    for (pane, rect) in laid {
        assert!(rect.right() <= columns, "pane {pane} at {rect:?} is off a tab of {size:?}");
        assert!(rect.bottom() <= rows, "pane {pane} at {rect:?} is off a tab of {size:?}");
        assert!(rect.columns >= COLUMNS_MIN, "pane {pane} at {rect:?} is narrower than a pane");
        assert!(rect.rows >= ROWS_MIN, "pane {pane} at {rect:?} is shorter than a pane");
        area += rect.area();
    }
    assert_eq!(area, Rect::whole(size).area(), "the panes of {laid:?} tile a tab of {size:?}");
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
        let mut tree = Tree::one(pane(1));
        tree.split(pane(1), Towards::Right, pane(2));
        tree.split(pane(2), Towards::Down, pane(3));
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
        let tree = Tree::one(pane(1));
        assert_eq!(tree.rects(size(80, 24)), [(pane(1), rect(0, 0, 80, 24))]);
        assert_eq!(rect(0, 0, 80, 24).inner(), size(78, 22));
        assert_eq!(rect(0, 0, COLUMNS_MIN, ROWS_MIN).inner(), size(3, 1));
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
        for (columns, rows) in [(80, 24), (81, 25), (11, 7), (10, 6), (200, 60)] {
            tiles(&tree, size(columns, rows));
        }
    }

    #[test]
    fn a_tab_too_small_for_its_panes_is_fitted_up_to_them() {
        let tree = three();
        assert!(tree.fits(size(10, 6)));
        assert!(!tree.fits(size(9, 6)));
        assert!(!tree.fits(size(10, 5)));
        assert_eq!(tree.fitted(size(4, 2)), size(10, 6));
        assert_eq!(tree.fitted(size(9, 40)), size(10, 40));
        assert_eq!(tree.fitted(size(80, 24)), size(80, 24));
        tiles(&tree, tree.fitted(size(4, 2)));
    }

    #[test]
    fn the_largest_pane_is_split_along_its_longer_side() {
        let tree = three();
        assert_eq!(tree.largest(size(80, 24)), (pane(1), Towards::Down));
        assert_eq!(tree.largest(size(200, 24)), (pane(1), Towards::Right));
        assert_eq!(Tree::one(pane(4)).largest(size(80, 24)), (pane(4), Towards::Right));
        let mut even = Tree::one(pane(1));
        even.split(pane(1), Towards::Right, pane(2));
        assert_eq!(even.largest(size(80, 24)).0, pane(1));
    }

    #[test]
    fn a_removed_pane_leaves_its_room_to_its_sibling() {
        let mut tree = three();
        assert_eq!(tree.remove(pane(2)), Removed::Out { heir: pane(3) });
        assert_eq!(tree.rects(size(80, 24))[1], (pane(3), rect(40, 0, 40, 24)));
        assert_eq!(tree.remove(pane(3)), Removed::Out { heir: pane(1) });
        assert_eq!(tree, Tree::one(pane(1)));
        assert_eq!(tree.remove(pane(1)), Removed::Last);
        let mut tree = three();
        assert_eq!(tree.remove(pane(1)), Removed::Out { heir: pane(2) });
        assert_eq!(tree.panes(), [pane(2), pane(3)]);
        assert_eq!(
            tree.rects(size(80, 24)),
            [(pane(2), rect(0, 0, 80, 12)), (pane(3), rect(0, 12, 80, 12))]
        );
    }

    #[test]
    fn a_tree_of_many_panes_keeps_its_order_and_tiles_as_panes_leave() {
        let at = size(400, 200);
        let mut tree = Tree::one(pane(1));
        for id in 2..=64 {
            let (largest, towards) = tree.largest(at);
            tree.split(largest, towards, pane(id));
            tiles(&tree, at);
        }
        let mut order = tree.panes();
        assert_eq!(order.len(), 64);
        for id in (1..=64).step_by(3) {
            assert!(matches!(tree.remove(pane(id)), Removed::Out { .. }));
            order.retain(|kept| *kept != pane(id));
            assert_eq!(tree.panes(), order, "the panes left keep their order");
            tiles(&tree, at);
        }
        for kept in order.iter().take(order.len() - 1) {
            assert!(matches!(tree.remove(*kept), Removed::Out { .. }));
            tiles(&tree, at);
        }
        assert_eq!(tree, Tree::one(*order.last().unwrap()));
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
        assert_eq!(tree.in_order(pane(3), Order::Next), Some(pane(1)));
        assert_eq!(tree.in_order(pane(1), Order::Previous), Some(pane(3)));
        assert_eq!(Tree::one(pane(1)).in_order(pane(1), Order::Next), None);
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
        let mut tree = Tree::one(pane(1));
        tree.split(pane(1), Towards::Right, pane(2));
        let at = size(20, 5);
        while tree.resize(at, pane(1), Direction::Right, Change::Grow) {}
        assert_eq!(tree.rects(at)[1].1, rect(15, 0, COLUMNS_MIN, 5));
        assert!(!tree.resize(at, pane(1), Direction::Right, Change::Grow));
        let cramped = size(2 * COLUMNS_MIN, 5);
        assert!(!tree.resize(cramped, pane(2), Direction::Left, Change::Grow));
    }

    #[test]
    fn a_swap_trades_two_panes_places() {
        let mut tree = three();
        tree.swap(pane(1), pane(3));
        assert_eq!(tree.panes(), [pane(3), pane(2), pane(1)]);
        let mut lone = Tree::one(pane(1));
        lone.swap(pane(1), pane(1));
        assert_eq!(lone, Tree::one(pane(1)));
    }
}
