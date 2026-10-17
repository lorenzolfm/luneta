//! Passing over the sidebar's column.
//!
//! zellij stops on the sidebar as on any pane. A key that moves the focus left
//! from the pane beside it lands in it, and one that moves it right into
//! another tab lands in that tab's placeholder, which the sidebar then takes
//! the place of, focus and all. Neither is where you meant to go: the sidebar
//! sends the focus on, to the tab before or to the pane beside the column, as
//! though the column were not there.
//!
//! It keeps the focus you ask for, with `focus`, and the focus a click gives
//! it. zellij hands a click that only focuses a pane to no one, but it ends the
//! hover over the sidebar first, so a click comes as the hover's end just
//! before the panes that show the sidebar focused. A key comes with none.

use zellij_tile::prelude::{Mouse, PaneId};

use crate::dock::TabId;
use crate::TICKS_PER_SECOND;

/// How long the focus you asked for may take to come, in ticks. A hidden
/// sidebar comes back before the dock moves the focus into it.
const WAIT: u64 = 2 * TICKS_PER_SECOND;

/// Where this client's focus is, in one report of the panes and the tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// A pane of its own; `beside` names it when it stands right of the
    /// column.
    Pane { tab: TabId, beside: Option<PaneId> },
    /// The floating panes, shown over the tab. Hiding them gives the focus
    /// back to the pane they covered.
    Floating,
    Sidebar { tab: TabId },
    Placeholder { tab: TabId },
}

/// Where the focus is to go on to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    /// Left again, into the tab before: where a key that moved it left from
    /// the pane beside the column would have taken it with no column there.
    Back,
    /// Right, to the pane beside the column.
    Beside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Visit {
    #[default]
    Out,
    /// You asked for the sidebar, at this frame, and the focus is on its way.
    Asked(u64),
    In,
    /// The focus landed in the column, and was sent on.
    Passing,
}

#[derive(Debug, Default)]
pub struct Pass {
    visit: Visit,
    /// The pane the focus was on last, outside the column: its tab, and the
    /// pane when it stands beside the column.
    from: Option<(TabId, Option<PaneId>)>,
    /// Whether the hover over the sidebar ended, or a click came through to
    /// it, since the report before.
    pointed: bool,
}

impl Pass {
    pub fn ask(&mut self, frame: u64) {
        if self.visit != Visit::In {
            self.visit = Visit::Asked(frame);
        }
    }

    /// Takes in a mouse event on the sidebar. zellij ends the hover over a
    /// plugin pane with a move to its last column, when a click focuses the
    /// pane and when the pointer leaves it; a click through comes itself. A
    /// pointer that moves on the sidebar, or scrolls it, is neither.
    pub fn mouse(&mut self, mouse: Mouse) {
        let ended = matches!(mouse, Mouse::Hover(0, column) if column == usize::from(u16::MAX));
        self.pointed |= ended || matches!(mouse, Mouse::LeftClick(..));
    }

    /// Whether the focus is in the column only on its way somewhere else.
    pub fn passing(&self) -> bool {
        self.visit == Visit::Passing
    }

    /// Takes in where the focus is after a report, and says where to send it
    /// on, if anywhere; `stands` says whether a pane is still in the tab. A
    /// focus sent on is not sent on again, whatever the reports that come
    /// before it gets there.
    pub fn see(
        &mut self,
        at: At,
        tabs: usize,
        frame: u64,
        stands: impl Fn(PaneId) -> bool,
    ) -> Option<On> {
        let pointed = std::mem::take(&mut self.pointed);
        if let Visit::Asked(asked) = self.visit {
            if frame.wrapping_sub(asked) > WAIT {
                self.visit = Visit::Out;
            }
        }
        let on = match (at, self.visit) {
            (At::Pane { tab, beside }, visit) => {
                self.from = Some((tab, beside));
                if !matches!(visit, Visit::Asked(_)) {
                    self.visit = Visit::Out;
                }
                return None;
            },
            // Over the floating panes, what the focus was in the pane they
            // cover holds, but one sent on has got where it went.
            (At::Floating, Visit::Passing) => {
                self.visit = Visit::Out;
                return None;
            },
            (At::Sidebar { .. }, Visit::Asked(_) | Visit::In) => {
                self.visit = Visit::In;
                return None;
            },
            // A click never takes the focus to another tab.
            (At::Sidebar { tab }, Visit::Out) if pointed && self.came_within(tab) => {
                self.visit = Visit::In;
                return None;
            },
            (At::Floating, _)
            | (At::Sidebar { .. } | At::Placeholder { .. }, Visit::Passing)
            | (At::Placeholder { .. }, Visit::Asked(_)) => return None,
            // One already in the column was not moved there: the sidebar left
            // the tab under it.
            (At::Placeholder { .. }, Visit::In) => On::Beside,
            (At::Sidebar { tab } | At::Placeholder { tab }, Visit::Out) => {
                if self.came_left(tab, stands) && tabs > 1 {
                    On::Back
                } else {
                    On::Beside
                }
            },
        };
        self.visit = Visit::Passing;
        Some(on)
    }

    fn came_within(&self, tab: TabId) -> bool {
        self.from.is_some_and(|(from, _)| from == tab)
    }

    /// Whether a key moved the focus left into the column, from the pane
    /// beside it. Not one whose pane is gone: zellij hands the focus of a pane
    /// that closes to the pane you were on before it, often the sidebar.
    fn came_left(&self, tab: TabId, stands: impl Fn(PaneId) -> bool) -> bool {
        let beside = self.from.and_then(|(from, beside)| beside.filter(|_| from == tab));
        beside.is_some_and(stands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Step {
        See(At),
        Ask,
        Point(Mouse),
        Wait(u64),
        /// The pane beside the column in this tab closes.
        Close(usize),
    }

    use Step::{Ask, Close, Point, See, Wait};

    /// The pane that stands beside the column in `tab`.
    const fn neighbour(tab: usize) -> PaneId {
        PaneId::Terminal(tab as u32)
    }

    const fn pane(tab: usize, beside: bool) -> Step {
        let beside = if beside { Some(neighbour(tab)) } else { None };
        See(At::Pane { tab: TabId(tab), beside })
    }

    const fn sidebar(tab: usize) -> Step {
        See(At::Sidebar { tab: TabId(tab) })
    }

    const fn placeholder(tab: usize) -> Step {
        See(At::Placeholder { tab: TabId(tab) })
    }

    const fn floating() -> Step {
        See(At::Floating)
    }

    /// A click that only focuses the sidebar: the end of the hover over it.
    fn click() -> Step {
        Point(Mouse::Hover(0, usize::from(u16::MAX)))
    }

    /// Every report the sidebar takes in, in two tabs, and where it sends the
    /// focus on after each.
    #[test]
    fn the_focus_passes_over_the_column_unless_you_asked_for_it() {
        let cases = [
            (
                "right, into the next tab: its placeholder, then the sidebar in its place",
                vec![pane(1, true), placeholder(2), sidebar(2), pane(2, true)],
                vec![None, Some(On::Beside), None, None],
            ),
            (
                "left, from the pane beside the column",
                vec![pane(2, true), sidebar(2), pane(1, false)],
                vec![None, Some(On::Back), None],
            ),
            (
                "a click on the sidebar, from the pane beside it",
                vec![pane(2, true), click(), sidebar(2), sidebar(2)],
                vec![None, None, None],
            ),
            (
                "a click that comes through to the sidebar, from the pane beside it",
                vec![pane(2, true), Point(Mouse::LeftClick(3, 4)), sidebar(2)],
                vec![None, None],
            ),
            (
                "the pointer moving on the sidebar, or scrolling it, does not keep a key",
                vec![
                    pane(2, true),
                    Point(Mouse::Hover(3, 4)),
                    Point(Mouse::ScrollDown(1)),
                    sidebar(2),
                ],
                vec![None, Some(On::Back)],
            ),
            (
                "the mouse on the sidebar does not keep a key that came from another tab",
                vec![pane(1, true), click(), sidebar(2)],
                vec![None, Some(On::Beside)],
            ),
            (
                "in the sidebar, with floating panes shown and hidden again",
                vec![pane(2, true), Ask, sidebar(2), floating(), sidebar(2)],
                vec![None, None, None, None],
            ),
            (
                "on to a tab with floating panes shown, and hidden again over its column",
                vec![pane(2, true), sidebar(2), floating(), placeholder(1)],
                vec![None, Some(On::Back), None, Some(On::Beside)],
            ),
            (
                "focus, through panes from before it came",
                vec![pane(2, true), Ask, pane(2, true), sidebar(2), sidebar(2), pane(2, true)],
                vec![None, None, None, None, None],
            ),
            (
                "focus that never came, and a key long after",
                vec![pane(2, true), Ask, Wait(WAIT + 1), sidebar(2)],
                vec![None, Some(On::Back)],
            ),
            (
                "in the sidebar, then on a placeholder, after a jump to another tab",
                vec![pane(2, true), Ask, sidebar(2), placeholder(1)],
                vec![None, None, Some(On::Beside)],
            ),
            (
                "out of the sidebar, then left into it again",
                vec![pane(2, true), Ask, sidebar(2), pane(2, true), sidebar(2)],
                vec![None, None, None, Some(On::Back)],
            ),
            (
                "from a pane that does not stand beside the column",
                vec![pane(2, false), sidebar(2)],
                vec![None, Some(On::Beside)],
            ),
            (
                "the pane beside the column closes, and zellij hands the sidebar the focus",
                vec![pane(2, true), Close(2), sidebar(2)],
                vec![None, Some(On::Beside)],
            ),
            (
                "in the sidebar as it leaves for another client's tab, on the placeholder it leaves",
                vec![pane(2, true), Ask, sidebar(2), placeholder(2)],
                vec![None, None, Some(On::Beside)],
            ),
        ];
        for (case, steps, sent) in cases {
            let mut pass = Pass::default();
            let mut frame = 0;
            let mut closed = Vec::new();
            let mut seen = Vec::new();
            for step in steps {
                match step {
                    See(at) => seen.push(pass.see(at, 2, frame, |pane| !closed.contains(&pane))),
                    Ask => pass.ask(frame),
                    Point(mouse) => pass.mouse(mouse),
                    Wait(ticks) => frame += ticks,
                    Close(tab) => closed.push(neighbour(tab)),
                }
            }
            assert_eq!(seen, sent, "{case}");
        }
    }

    #[test]
    fn left_in_the_only_tab_goes_back_beside() {
        let mut pass = Pass::default();
        let beside = At::Pane { tab: TabId(1), beside: Some(neighbour(1)) };
        assert_eq!(pass.see(beside, 1, 0, |_| true), None);
        assert_eq!(pass.see(At::Sidebar { tab: TabId(1) }, 1, 0, |_| true), Some(On::Beside));
    }
}
