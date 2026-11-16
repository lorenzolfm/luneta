use std::cmp::Reverse;
use std::num::NonZeroU16;

use super::Id;

pub const FRAME_CELLS: u16 = 1;

pub const COLUMNS_MIN: u16 = 5;

pub const ROWS_MIN: u16 = 3;

pub const PANES_MAX: usize = 64;

pub const SIDE_MAX: u16 = 1000;

const SHARES: u32 = 1_000_000;

const STEP_PERCENT: u32 = 5;

pub const NODES_MAX: usize = 2 * PANES_MAX - 1;

const ROOT: usize = 0;

const _: () = assert!(COLUMNS_MIN > 2 * FRAME_CELLS);
const _: () = assert!(ROWS_MIN > 2 * FRAME_CELLS);
const _: () = assert!(PANES_MAX * COLUMNS_MIN as usize <= SIDE_MAX as usize);
const _: () = assert!(PANES_MAX * ROWS_MIN as usize <= SIDE_MAX as usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub rows: NonZeroU16,
}

impl Size {
    pub fn of(columns: u32, rows: u32) -> Result<Size, String> {
        let side = |name: &str, cells: u32| {
            let over = || format!("{name} is over {SIDE_MAX}");
            let cells = u16::try_from(cells).ok().filter(|cells| *cells <= SIDE_MAX);
            NonZeroU16::new(cells.ok_or_else(over)?).ok_or_else(|| format!("{name} is 0"))
        };
        Ok(Size { columns: side("columns", columns)?, rows: side("rows", rows)? })
    }

    #[must_use]
    pub fn get(self) -> (u16, u16) {
        (self.columns.get(), self.rows.get())
    }

    #[must_use]
    pub fn smallest(sizes: impl IntoIterator<Item = Size>) -> Option<Size> {
        sizes.into_iter().reduce(|one, other| Size {
            columns: one.columns.min(other.columns),
            rows: one.rows.min(other.rows),
        })
    }
}

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

    fn overlaps(self, other: Rect) -> bool {
        self.column < other.right()
            && other.column < self.right()
            && self.row < other.bottom()
            && other.row < self.bottom()
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

    fn raised(self, size: Size) -> Size {
        let raised = |cells: NonZeroU16, least: u16| {
            cells.max(NonZeroU16::new(least).expect("a pane is a cell or more each way"))
        };
        let raised =
            Size { columns: raised(size.columns, self.columns), rows: raised(size.rows, self.rows) };
        assert!(self.fits(raised), "{raised:?} holds the panes of the tree");
        raised
    }
}

pub trait Leaf {
    fn id(&self) -> Id;
}

impl Leaf for Id {
    fn id(&self) -> Id {
        *self
    }
}

#[derive(Clone, Debug)]
pub struct Tree<L> {
    nodes: Vec<Node<L>>,
}

impl<L: Leaf + PartialEq> PartialEq for Tree<L> {
    fn eq(&self, other: &Tree<L>) -> bool {
        self.preorder() == other.preorder()
    }
}

impl<L: Leaf + Eq> Eq for Tree<L> {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node<L> {
    Pane(L),
    Split(Split),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Split {
    towards: Towards,
    share: u32,
    halves: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preordered<L> {
    Split { towards: Towards, share: u32 },
    Pane(L),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Removed<L> {
    Last,
    Out { gone: L, heir: Id },
}

#[must_use]
pub fn lone(size: Size) -> Size {
    Rect::whole(Least::PANE.raised(size)).inner()
}

impl<L: Leaf> Tree<L> {
    #[must_use]
    pub fn one(pane: L) -> Tree<L> {
        Tree { nodes: vec![Node::Pane(pane)] }
    }

    pub fn of_preorder(preorder: Vec<Preordered<L>>) -> Result<Tree<L>, String> {
        if preorder.len() > NODES_MAX {
            return Err(format!("a view holds at most {PANES_MAX} terminals"));
        }
        let mut nodes: Vec<Option<Node<L>>> = Vec::with_capacity(preorder.len());
        nodes.push(None);
        let mut open = vec![ROOT];
        for found in preorder {
            let Some(slot) = open.pop() else {
                return Err("a view holds nodes after its last terminal".to_owned());
            };
            nodes[slot] = Some(match found {
                Preordered::Pane(leaf) => Node::Pane(leaf),
                Preordered::Split { towards, share } => {
                    if share > SHARES {
                        return Err(format!("a split's share of {share} is over {SHARES}"));
                    }
                    let halves = nodes.len();
                    nodes.extend([None, None]);
                    open.extend([halves + 1, halves]);
                    Node::Split(Split { towards, share, halves })
                },
            });
        }
        if !open.is_empty() {
            return Err("a split of a view lacks a half".to_owned());
        }
        let filled = "a preorder that ends with no open slot fills every node";
        let nodes: Vec<Node<L>> = nodes.into_iter().map(|node| node.expect(filled)).collect();
        let tree = Tree { nodes };
        let mut panes = tree.panes();
        let count = panes.len();
        panes.sort_unstable();
        panes.dedup();
        if panes.len() != count {
            return Err("a view holds a terminal twice".to_owned());
        }
        tree.assert_shaped();
        Ok(tree)
    }

    #[must_use]
    pub fn preorder(&self) -> Vec<Preordered<&L>> {
        let mut preorder = Vec::with_capacity(self.nodes.len());
        let mut stack = Vec::with_capacity(self.nodes.len());
        stack.push(ROOT);
        for _ in 0..self.nodes.len() {
            let node = stack.pop().expect("a tree visits each of its nodes once");
            match &self.nodes[node] {
                Node::Pane(leaf) => preorder.push(Preordered::Pane(leaf)),
                Node::Split(split) => {
                    preorder.push(Preordered::Split { towards: split.towards, share: split.share });
                    stack.push(split.second());
                    stack.push(split.first());
                },
            }
        }
        assert!(stack.is_empty(), "a tree visits each of its nodes once");
        preorder
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len().div_ceil(2)
    }

    #[must_use]
    pub fn panes(&self) -> Vec<Id> {
        self.leaves().map(Leaf::id).collect()
    }

    pub fn leaves(&self) -> impl Iterator<Item = &L> {
        self.order().into_iter().map(|node| self.leaf_at(node))
    }

    #[must_use]
    pub fn holds(&self, pane: Id) -> bool {
        self.leaf(pane).is_some()
    }

    #[must_use]
    pub fn get(&self, pane: Id) -> Option<&L> {
        Some(self.leaf_at(self.leaf(pane)?))
    }

    pub fn get_mut(&mut self, pane: Id) -> Option<&mut L> {
        let node = self.leaf(pane)?;
        match &mut self.nodes[node] {
            Node::Pane(leaf) => Some(leaf),
            Node::Split(_) => unreachable!("node {node} is a pane's leaf"),
        }
    }

    #[must_use]
    pub fn into_lone(mut self) -> L {
        assert_eq!(self.nodes.len(), 1, "a lone pane's tree holds only it");
        match self.nodes.pop() {
            Some(Node::Pane(leaf)) => leaf,
            Some(Node::Split(_)) | None => unreachable!("the root of a tree of one is a pane"),
        }
    }

    #[must_use]
    pub fn fitted(&self, size: Size) -> Size {
        self.leasts()[ROOT].raised(size)
    }

    #[must_use]
    pub fn laid(&self, size: Size) -> Vec<(&L, Rect)> {
        let size = self.fitted(size);
        let rects = self.node_rects(&self.leasts(), size);
        let laid: Vec<(&L, Rect)> =
            self.order().into_iter().map(|node| (self.leaf_at(node), rects[node])).collect();
        assert_tiled(&laid, size);
        laid
    }

    #[must_use]
    pub fn rects(&self, size: Size) -> Vec<(Id, Rect)> {
        self.laid(size).into_iter().map(|(leaf, rect)| (leaf.id(), rect)).collect()
    }

    #[must_use]
    pub fn room(&self, at: Id, towards: Towards, new: Id, size: Size) -> Option<Size> {
        if self.len() >= PANES_MAX {
            return None;
        }
        let size = self.fitted(size);
        let mut grown = self.shape();
        grown.grafted(at, towards, new);
        grown.leasts()[ROOT].fits(size).then(|| {
            let laid = grown.rects(size).into_iter().find(|(pane, _)| *pane == new);
            laid.expect("a grafted pane is laid out").1.inner()
        })
    }

    #[must_use]
    pub fn room_beside_largest(&self, new: Id, size: Size) -> Option<(Id, Towards, Size)> {
        let (largest, towards) = self.largest(size);
        [towards, towards.across()]
            .into_iter()
            .find_map(|towards| Some((largest, towards, self.room(largest, towards, new, size)?)))
    }

    pub fn split(&mut self, at: Id, towards: Towards, new: L, size: Size) {
        let id = new.id();
        let size = self.fitted(size);
        self.grafted(at, towards, new);
        assert!(self.leasts()[ROOT].fits(size), "pane {id} has room in {size:?}");
    }

    #[must_use]
    pub fn largest(&self, size: Size) -> (Id, Towards) {
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

    #[must_use]
    pub fn heir(&self, pane: Id) -> Option<Id> {
        let node = self.leaf(pane).expect("a pane with an heir is in the tree");
        let (_, sibling) = self.halves_of(node)?;
        let panes = self.panes();
        let place = panes.iter().position(|found| *found == pane);
        let place = place.expect("a pane of the tree is in its order");
        let heir = if sibling > node { panes[place + 1] } else { panes[place - 1] };
        assert_ne!(heir, pane, "a pane is no heir of itself");
        Some(heir)
    }

    pub fn remove(&mut self, pane: Id) -> Removed<L> {
        let node = self.leaf(pane).expect("a removed pane is in the tree");
        let Some(heir) = self.heir(pane) else {
            assert_eq!(self.nodes.len(), 1, "the root is a pane only in a tree of one");
            return Removed::Last;
        };
        let (parent, sibling) = self.halves_of(node).expect("a pane with an heir has a parent");
        let Node::Split(split) = self.nodes[parent] else {
            unreachable!("node {parent} is a parent, so a split");
        };
        self.nodes.swap(parent, sibling);
        let gone = self.compact(split.halves, node);
        self.assert_shaped();
        assert!(!self.holds(pane), "pane {pane} left the tree");
        assert!(self.holds(heir), "pane {heir} took its sibling's room");
        Removed::Out { gone, heir }
    }

    pub fn swap(&mut self, one: Id, other: Id) {
        let one = self.leaf(one).expect("a swapped pane is in the tree");
        let other = self.leaf(other).expect("a swapped pane is in the tree");
        self.nodes.swap(one, other);
    }

    #[must_use]
    pub fn neighbour(&self, pane: Id, direction: Direction, size: Size) -> Option<Id> {
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
    pub fn in_order(&self, pane: Id, order: Order) -> Option<Id> {
        let panes = self.panes();
        let place = panes.iter().position(|found| *found == pane);
        let place = place.expect("a pane of the tree is in its order");
        (panes.len() > 1).then(|| panes[order.stepped(place, panes.len())])
    }

    #[must_use]
    pub fn resize(&mut self, pane: Id, direction: Direction, change: Change, size: Size) -> bool {
        let (towards, half) = direction.border();
        let leasts = self.leasts();
        let rects = self.node_rects(&leasts, self.fitted(size));
        let parents = self.parents();
        let mut node = self.leaf(pane).expect("a resized pane is in the tree");
        for _ in 0..self.nodes.len() {
            let Some(parent) = parents[node] else {
                return false;
            };
            let Node::Split(split) = &mut self.nodes[parent] else {
                unreachable!("node {parent} is a parent, so a split");
            };
            let held = if split.first() == node { Side::First } else { Side::Second };
            if (split.towards, held) == (towards, half) {
                let extent = rects[parent].extent(towards);
                let bounds = split.bounds(extent, leasts[split.first()], leasts[split.second()]);
                return split.moved(extent, bounds, half, change);
            }
            node = parent;
        }
        unreachable!("a walk up a tree of {} nodes reaches its root", self.nodes.len());
    }

    fn shape(&self) -> Tree<Id> {
        let nodes = self.nodes.iter().map(|node| match node {
            Node::Pane(leaf) => Node::Pane(leaf.id()),
            Node::Split(split) => Node::Split(*split),
        });
        Tree { nodes: nodes.collect() }
    }

    fn grafted(&mut self, at: Id, towards: Towards, new: L) {
        assert!(!self.holds(new.id()), "pane {} is split into a tree that holds it", new.id());
        assert!(self.nodes.len() + 2 <= NODES_MAX, "a tree holds at most {PANES_MAX} panes");
        let node = self.leaf(at).expect("a split pane is in the tree");
        let halves = self.nodes.len();
        let split = Node::Split(Split { towards, share: SHARES / 2, halves });
        let old = std::mem::replace(&mut self.nodes[node], split);
        self.nodes.push(old);
        self.nodes.push(Node::Pane(new));
        self.assert_shaped();
    }

    fn halves_of(&self, node: usize) -> Option<(usize, usize)> {
        if node == ROOT {
            return None;
        }
        let parent = self.parents()[node].expect("a node under the root has a parent");
        let Node::Split(split) = self.nodes[parent] else {
            unreachable!("node {parent} is a parent, so a split");
        };
        if split.first() == node {
            return Some((parent, split.second()));
        }
        assert_eq!(split.second(), node, "a node is one half of its parent");
        Some((parent, split.first()))
    }

    fn leaf(&self, pane: Id) -> Option<usize> {
        self.nodes.iter().position(|node| matches!(node, Node::Pane(leaf) if leaf.id() == pane))
    }

    fn leaf_at(&self, node: usize) -> &L {
        match &self.nodes[node] {
            Node::Pane(leaf) => leaf,
            Node::Split(_) => unreachable!("node {node} is laid out as a pane"),
        }
    }

    fn order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.len());
        let mut stack = Vec::with_capacity(self.nodes.len());
        stack.push(ROOT);
        for _ in 0..self.nodes.len() {
            let node = stack.pop().expect("a tree visits each of its nodes once");
            match &self.nodes[node] {
                Node::Pane(_) => order.push(node),
                Node::Split(split) => {
                    stack.push(split.second());
                    stack.push(split.first());
                },
            }
        }
        assert!(stack.is_empty(), "a tree visits each of its nodes once");
        assert_eq!(order.len(), self.len(), "a tree of splits in two");
        order
    }

    fn parents(&self) -> Vec<Option<usize>> {
        let mut parents = vec![None; self.nodes.len()];
        for (node, found) in self.nodes.iter().enumerate() {
            if let Node::Split(split) = found {
                assert!(split.halves > node, "node {} comes after its parent {node}", split.halves);
                for half in [split.first(), split.second()] {
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
                assert!(split.halves > node, "node {} comes after its parent {node}", split.halves);
                leasts[node] = leasts[split.first()].joined(leasts[split.second()], split.towards);
            }
        }
        leasts
    }

    fn node_rects(&self, leasts: &[Least], size: Size) -> Vec<Rect> {
        assert_eq!(leasts.len(), self.nodes.len(), "a least size for every node");
        assert!(leasts[ROOT].fits(size), "{size:?} holds the panes of the tree");
        let mut rects = vec![Rect::whole(size); self.nodes.len()];
        for (node, found) in self.nodes.iter().enumerate() {
            if let Node::Split(split) = found {
                let (first, second) = (leasts[split.first()], leasts[split.second()]);
                let (first, second) = split.divided(rects[node], first, second);
                rects[split.first()] = first;
                rects[split.second()] = second;
            }
        }
        rects
    }

    fn compact(&mut self, halves: usize, leaving: usize) -> L {
        let shifted = |node: usize| {
            assert!(node < halves || node > halves + 1, "node {node} is gone");
            if node > halves { node - 2 } else { node }
        };
        let count = self.nodes.len();
        let mut gone = None;
        let mut kept = Vec::with_capacity(count - 2);
        for (node, found) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            match found {
                Node::Pane(leaf) if node == leaving => gone = Some(leaf),
                _ if node == halves || node == halves + 1 => {},
                pane @ Node::Pane(_) => kept.push(pane),
                Node::Split(split) => {
                    kept.push(Node::Split(Split { halves: shifted(split.halves), ..split }));
                },
            }
        }
        assert_eq!(kept.len() + 2, count, "two nodes left the tree");
        self.nodes = kept;
        gone.expect("the leaving pane was one of the two halves")
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
    fn first(self) -> usize {
        self.halves
    }

    fn second(self) -> usize {
        self.halves + 1
    }

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

    fn divided(self, rect: Rect, first: Least, second: Least) -> (Rect, Rect) {
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

fn assert_tiled<L: Leaf>(laid: &[(&L, Rect)], size: Size) {
    let (columns, rows) = size.get();
    let mut area = 0;
    for (place, (leaf, rect)) in laid.iter().enumerate() {
        let pane = leaf.id();
        assert!(rect.right() <= columns, "pane {pane} at {rect:?} is off a view of {size:?}");
        assert!(rect.bottom() <= rows, "pane {pane} at {rect:?} is off a view of {size:?}");
        assert!(rect.columns >= COLUMNS_MIN, "pane {pane} at {rect:?} is narrower than a pane");
        assert!(rect.rows >= ROWS_MIN, "pane {pane} at {rect:?} is shorter than a pane");
        for (other, over) in &laid[place + 1..] {
            let other = other.id();
            assert!(!rect.overlaps(*over), "pane {other} at {over:?} is over pane {pane}");
        }
        area += rect.area();
    }
    assert_eq!(area, Rect::whole(size).area(), "the panes tile a view of {size:?}");
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

    fn pane(id: u64) -> Id {
        Id::of(id).unwrap()
    }

    fn size(columns: u16, rows: u16) -> Size {
        Size { columns: NonZeroU16::new(columns).unwrap(), rows: NonZeroU16::new(rows).unwrap() }
    }

    fn rect(column: u16, row: u16, columns: u16, rows: u16) -> Rect {
        Rect { column, row, columns, rows }
    }

    fn three(at: Size) -> Tree<Id> {
        let mut tree = Tree::one(pane(1));
        tree.split(pane(1), Towards::Right, pane(2), at);
        tree.split(pane(2), Towards::Down, pane(3), at);
        tree
    }

    fn tiles(tree: &Tree<Id>, at: Size) {
        let rects = tree.rects(at);
        let (columns, rows) = tree.fitted(at).get();
        let mut covered = vec![0u8; usize::from(columns) * usize::from(rows)];
        for (_, rect) in &rects {
            for row in rect.row..rect.bottom() {
                for column in rect.column..rect.right() {
                    covered[usize::from(row) * usize::from(columns) + usize::from(column)] += 1;
                }
            }
        }
        assert!(covered.iter().all(|count| *count == 1), "{rects:?} tile {at:?}");
    }

    #[test]
    fn a_tree_of_as_many_panes_as_it_may_has_room_for_none() {
        let at = size(SIDE_MAX, SIDE_MAX);
        let mut tree = Tree::one(pane(1));
        for new in 2..=u64::try_from(PANES_MAX).unwrap() {
            let (beside, towards, _) = tree.room_beside_largest(pane(new), at).unwrap();
            tree.split(beside, towards, pane(new), at);
        }
        assert_eq!(tree.len(), PANES_MAX);
        assert_eq!(tree.room_beside_largest(pane(1000), at), None);
        assert_eq!(tree.room(pane(1), Towards::Right, pane(1000), at), None);
    }

    #[test]
    fn a_size_is_bounded_on_each_side() {
        assert_eq!(Size::of(80, 24), Ok(size(80, 24)));
        assert_eq!(Size::of(1000, 1), Ok(size(1000, 1)));
        assert_eq!(Size::of(0, 24), Err("columns is 0".to_owned()));
        assert_eq!(Size::of(80, 1001), Err("rows is over 1000".to_owned()));
        assert_eq!(Size::of(1 << 16, 24), Err("columns is over 1000".to_owned()));
        let smallest = Size::smallest([size(80, 10), size(40, 24)]);
        assert_eq!(smallest, Some(size(40, 10)));
        assert_eq!(Size::smallest([]), None);
    }

    #[test]
    fn a_lone_pane_takes_the_view_and_its_frame_takes_a_cell_each_side() {
        let tree = Tree::one(pane(1));
        assert_eq!(tree.rects(size(80, 24)), [(pane(1), rect(0, 0, 80, 24))]);
        assert_eq!(rect(0, 0, 80, 24).inner(), size(78, 22));
        assert_eq!(rect(0, 0, COLUMNS_MIN, ROWS_MIN).inner(), size(3, 1));
        assert_eq!(lone(size(80, 24)), size(78, 22));
        assert_eq!(lone(size(1, 1)), size(3, 1));
        assert_eq!(tree.fitted(size(1, 1)), size(COLUMNS_MIN, ROWS_MIN));
    }

    #[test]
    fn splits_halve_the_pane_split_and_tile_the_view() {
        let tree = three(size(80, 24));
        assert_eq!(
            tree.rects(size(80, 24)),
            [
                (pane(1), rect(0, 0, 40, 24)),
                (pane(2), rect(40, 0, 40, 12)),
                (pane(3), rect(40, 12, 40, 12)),
            ]
        );
        assert_eq!(tree.panes(), [pane(1), pane(2), pane(3)]);
        assert_eq!(tree.len(), 3);
        for (columns, rows) in [(80, 24), (81, 25), (11, 7), (10, 6), (200, 60)] {
            assert_eq!(tree.fitted(size(columns, rows)), size(columns, rows));
            tiles(&tree, size(columns, rows));
        }
    }

    #[test]
    fn a_view_too_small_for_its_panes_is_fitted_up_to_them() {
        let tree = three(size(80, 24));
        for (asked, fitted) in [((4, 2), (10, 6)), ((9, 40), (10, 40)), ((80, 24), (80, 24))] {
            assert_eq!(tree.fitted(size(asked.0, asked.1)), size(fitted.0, fitted.1));
            tiles(&tree, size(asked.0, asked.1));
        }
    }

    #[test]
    fn a_pane_has_room_only_where_the_view_holds_it() {
        let at = size(10, 6);
        let tree = three(at);
        assert_eq!(tree.room(pane(1), Towards::Down, pane(4), at), Some(size(3, 1)));
        assert_eq!(tree.room(pane(1), Towards::Right, pane(4), at), None);
        let beside = tree.room_beside_largest(pane(4), at);
        assert_eq!(beside, Some((pane(1), Towards::Down, size(3, 1))));
        let full = Tree::one(pane(1));
        assert_eq!(full.room_beside_largest(pane(2), size(COLUMNS_MIN, ROWS_MIN)), None);
        let taller = size(COLUMNS_MIN, 2 * ROWS_MIN);
        let beside = full.room_beside_largest(pane(2), taller);
        assert_eq!(beside, Some((pane(1), Towards::Down, size(3, 1))));
    }

    #[test]
    fn the_largest_pane_is_split_along_its_longer_side() {
        let tree = three(size(80, 24));
        assert_eq!(tree.largest(size(80, 24)), (pane(1), Towards::Down));
        assert_eq!(tree.largest(size(200, 24)), (pane(1), Towards::Right));
        assert_eq!(Tree::one(pane(4)).largest(size(80, 24)), (pane(4), Towards::Right));
        let mut even = Tree::one(pane(1));
        even.split(pane(1), Towards::Right, pane(2), size(80, 24));
        assert_eq!(even.largest(size(80, 24)).0, pane(1));
    }

    #[test]
    fn a_removed_pane_leaves_its_room_to_its_sibling() {
        let at = size(80, 24);
        let mut tree = three(at);
        assert_eq!([1, 2, 3].map(|id| tree.heir(pane(id))), [2, 3, 2].map(|id| Some(pane(id))));
        assert_eq!(tree.remove(pane(2)), Removed::Out { gone: pane(2), heir: pane(3) });
        assert_eq!(tree.rects(at)[1], (pane(3), rect(40, 0, 40, 24)));
        assert_eq!(tree.remove(pane(3)), Removed::Out { gone: pane(3), heir: pane(1) });
        assert_eq!(tree, Tree::one(pane(1)));
        assert_eq!(tree.heir(pane(1)), None);
        assert_eq!(tree.remove(pane(1)), Removed::Last);
        assert_eq!(tree.into_lone(), pane(1));
        let mut tree = three(at);
        assert_eq!(tree.remove(pane(1)), Removed::Out { gone: pane(1), heir: pane(2) });
        assert_eq!(tree.panes(), [pane(2), pane(3)]);
        let rects = tree.rects(at);
        assert_eq!(rects, [(pane(2), rect(0, 0, 80, 12)), (pane(3), rect(0, 12, 80, 12))]);
    }

    #[test]
    fn a_tree_of_many_panes_keeps_its_order_and_tiles_as_panes_leave() {
        let at = size(400, 200);
        let mut tree = Tree::one(pane(1));
        for id in 2..=64 {
            let (largest, towards, _) = tree.room_beside_largest(pane(id), at).unwrap();
            tree.split(largest, towards, pane(id), at);
            tiles(&tree, at);
        }
        let mut order = tree.panes();
        assert_eq!(order.len(), PANES_MAX);
        for id in (1..=64).step_by(3) {
            assert!(matches!(tree.remove(pane(id)), Removed::Out { gone, .. } if gone == pane(id)));
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
        let at = size(80, 24);
        let tree = three(at);
        assert_eq!(tree.neighbour(pane(1), Direction::Right, at), Some(pane(2)));
        assert_eq!(tree.neighbour(pane(3), Direction::Left, at), Some(pane(1)));
        assert_eq!(tree.neighbour(pane(2), Direction::Down, at), Some(pane(3)));
        assert_eq!(tree.neighbour(pane(3), Direction::Up, at), Some(pane(2)));
        assert_eq!(tree.neighbour(pane(1), Direction::Left, at), None);
        assert_eq!(tree.neighbour(pane(1), Direction::Up, at), None);
        assert_eq!(tree.in_order(pane(3), Order::Next), Some(pane(1)));
        assert_eq!(tree.in_order(pane(1), Order::Previous), Some(pane(3)));
        assert_eq!(Tree::one(pane(1)).in_order(pane(1), Order::Next), None);
    }

    #[test]
    fn a_resize_moves_the_nearest_border_on_that_side() {
        let at = size(80, 24);
        let mut tree = three(at);
        assert!(tree.resize(pane(3), Direction::Left, Change::Grow, at));
        assert_eq!(tree.rects(at)[0].1, rect(0, 0, 36, 24));
        assert!(tree.resize(pane(3), Direction::Up, Change::Grow, at));
        assert_eq!(tree.rects(at)[2].1, rect(36, 11, 44, 13));
        assert!(tree.resize(pane(1), Direction::Right, Change::Shrink, at));
        assert_eq!(tree.rects(at)[0].1, rect(0, 0, 32, 24));
        assert!(!tree.resize(pane(1), Direction::Left, Change::Grow, at));
        assert!(!tree.resize(pane(2), Direction::Up, Change::Grow, at));
        tiles(&tree, at);
    }

    #[test]
    fn a_border_stops_where_a_pane_would_be_too_small() {
        let at = size(20, 5);
        let mut tree = Tree::one(pane(1));
        tree.split(pane(1), Towards::Right, pane(2), at);
        while tree.resize(pane(1), Direction::Right, Change::Grow, at) {}
        assert_eq!(tree.rects(at)[1].1, rect(15, 0, COLUMNS_MIN, 5));
        assert!(!tree.resize(pane(1), Direction::Right, Change::Grow, at));
        let narrow = size(2 * COLUMNS_MIN, 5);
        assert!(!tree.resize(pane(2), Direction::Left, Change::Grow, narrow));
    }

    #[test]
    fn a_swap_trades_two_panes_places() {
        let mut tree = three(size(80, 24));
        tree.swap(pane(1), pane(3));
        assert_eq!(tree.panes(), [pane(3), pane(2), pane(1)]);
        let mut lone = Tree::one(pane(1));
        lone.swap(pane(1), pane(1));
        assert_eq!(lone, Tree::one(pane(1)));
    }

    #[test]
    fn a_tree_holds_its_leaves_and_lends_them() {
        let mut tree = three(size(80, 24));
        assert_eq!(tree.get(pane(2)), Some(&pane(2)));
        assert_eq!(tree.get(pane(9)), None);
        assert_eq!(tree.get_mut(pane(3)).copied(), Some(pane(3)));
        assert_eq!(tree.leaves().copied().collect::<Vec<_>>(), [pane(1), pane(2), pane(3)]);
        assert!(rect(0, 0, 4, 4).overlaps(rect(3, 3, 4, 4)));
        assert!(!rect(0, 0, 4, 4).overlaps(rect(4, 0, 4, 4)));
    }

    #[test]
    fn a_tree_reads_back_from_its_preorder_whatever_its_history() {
        let at = size(80, 24);
        let mut tree = three(at);
        tree.split(pane(1), Towards::Down, pane(4), at);
        assert!(tree.resize(pane(4), Direction::Up, Change::Grow, at));
        assert!(matches!(tree.remove(pane(2)), Removed::Out { .. }));
        let preorder: Vec<Preordered<Id>> = tree
            .preorder()
            .into_iter()
            .map(|node| match node {
                Preordered::Pane(leaf) => Preordered::Pane(*leaf),
                Preordered::Split { towards, share } => Preordered::Split { towards, share },
            })
            .collect();
        let split = |towards| Preordered::Split { towards, share: SHARES / 2 };
        let grown = Preordered::Split { towards: Towards::Down, share: 458_333 };
        let want = [
            split(Towards::Right),
            grown,
            Preordered::Pane(pane(1)),
            Preordered::Pane(pane(4)),
            Preordered::Pane(pane(3)),
        ];
        assert_eq!(preorder, want);
        let read = Tree::of_preorder(preorder).unwrap();
        assert_eq!(read, tree);
        assert_eq!(read.rects(at), tree.rects(at));
        assert_ne!(read, three(at));
    }

    #[test]
    fn a_preorder_that_is_no_tree_is_refused() {
        let split = Preordered::Split { towards: Towards::Right, share: SHARES / 2 };
        let leaf = |id| Preordered::Pane(pane(id));
        let over = Preordered::Split { towards: Towards::Right, share: SHARES + 1 };
        let refused: [(Vec<Preordered<Id>>, &str); 5] = [
            (vec![], "lacks a half"),
            (vec![split, leaf(1)], "lacks a half"),
            (vec![leaf(1), leaf(2)], "after its last terminal"),
            (vec![split, leaf(1), leaf(1)], "a terminal twice"),
            (vec![over, leaf(1), leaf(2)], "is over 1000000"),
        ];
        for (preorder, why) in refused {
            let read = Tree::of_preorder(preorder.clone());
            assert!(read.as_ref().unwrap_err().contains(why), "{preorder:?}: {read:?}");
        }
        let many = (0..=NODES_MAX).map(|_| split).collect();
        assert!(Tree::of_preorder(many).unwrap_err().contains("at most 64 terminals"));
    }
}
