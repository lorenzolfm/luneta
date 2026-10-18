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
//! before the panes that show the sidebar focused. A key comes with none, and
//! the hover's end when the pointer leaves comes long before any key, nearly
//! always. zellij says when a key comes, before it acts on it: one since the
//! hover's end keeps the focus, but the click acts on nothing.

use zellij_tile::prelude::PaneId;

use crate::dock::TabId;
use crate::TICKS_PER_SECOND;

/// How long the focus you asked for may take to come, in ticks. A hidden
/// sidebar comes back before the dock moves the focus into it.
const WAIT: u64 = 2 * TICKS_PER_SECOND;

/// How long the panes that show a click's focus may take to come after the
/// hover it ends, in ticks. zellij sends both at once.
const CLICK: u64 = TICKS_PER_SECOND / 5;

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

/// What the sidebar does after a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// Nothing: the focus stays where it is.
    Stay,
    /// The focus stays, and the click that brought it in acts on what it was
    /// on: zellij handed that click to no one.
    Press,
    /// Sends the focus left again, into the tab before: where a key that
    /// moved it left from the pane beside the column would have taken it with
    /// no column there.
    Back,
    /// Sends the focus right, to the pane beside the column.
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

/// The end of the hover over the sidebar, or a click that came through to it.
#[derive(Clone, Copy, Debug)]
struct Pointed {
    /// The frame it came at.
    at: u64,
    /// Whether a key came since.
    keyed: bool,
}

#[derive(Debug, Default)]
pub struct Pass {
    visit: Visit,
    /// The pane the focus was on last, outside the column: its tab, and the
    /// pane when it stands beside the column.
    from: Option<(TabId, Option<PaneId>)>,
    /// The last click on the sidebar since the report before.
    pointed: Option<Pointed>,
}

impl Pass {
    pub fn ask(&mut self, frame: u64) {
        if self.visit != Visit::In {
            self.visit = Visit::Asked(frame);
        }
    }

    /// Takes in a click on the sidebar, at `frame`: the end of the hover over
    /// it, as a click that only focuses it comes, or a click that comes
    /// through.
    pub fn point(&mut self, frame: u64) {
        self.pointed = Some(Pointed { at: frame, keyed: false });
    }

    /// Takes in a key, or any input but the mouse's. zellij tells every plugin
    /// of one before it acts on it, and of a click only after the panes the
    /// click changed: a key since the hover's end may be what brings the focus
    /// in, and the click then acts on nothing. The focus stays, as a click's
    /// does, since zellij tells of a key typed anywhere, even as a click comes.
    pub fn input(&mut self) {
        if let Some(pointed) = &mut self.pointed {
            pointed.keyed = true;
        }
    }

    /// Whether the focus is in the column only on its way somewhere else.
    pub fn passing(&self) -> bool {
        self.visit == Visit::Passing
    }

    /// Takes in where the focus is after a report, and says what to do: send
    /// it on, act on the click that brought it, or nothing; `stands` says
    /// whether a pane is still in the tab. A focus sent on is not sent on
    /// again, whatever the reports that come before it gets there.
    pub fn see(
        &mut self,
        at: At,
        tabs: usize,
        frame: u64,
        stands: impl Fn(PaneId) -> bool,
    ) -> Then {
        let pointed = self.pointed.take().filter(|pointed| frame.wrapping_sub(pointed.at) <= CLICK);
        if let Visit::Asked(asked) = self.visit {
            if frame.wrapping_sub(asked) > WAIT {
                self.visit = Visit::Out;
            }
        }
        let sent = match (at, self.visit) {
            (At::Pane { tab, beside }, visit) => {
                self.from = Some((tab, beside));
                if !matches!(visit, Visit::Asked(_)) {
                    self.visit = Visit::Out;
                }
                return Then::Stay;
            },
            // Over the floating panes, what the focus was in the pane they
            // cover holds, but one sent on has got where it went.
            (At::Floating, Visit::Passing) => {
                self.visit = Visit::Out;
                return Then::Stay;
            },
            (At::Sidebar { .. }, Visit::Asked(_) | Visit::In) => {
                self.visit = Visit::In;
                return Then::Stay;
            },
            // A click never takes the focus to another tab.
            (At::Sidebar { tab }, Visit::Out) if pointed.is_some() && self.came_within(tab) => {
                self.visit = Visit::In;
                return if pointed.is_some_and(|pointed| pointed.keyed) {
                    Then::Stay
                } else {
                    Then::Press
                };
            },
            (At::Floating, _)
            | (At::Sidebar { .. } | At::Placeholder { .. }, Visit::Passing)
            | (At::Placeholder { .. }, Visit::Asked(_)) => return Then::Stay,
            // One already in the column was not moved there: the sidebar left
            // the tab under it.
            (At::Placeholder { .. }, Visit::In) => Then::Beside,
            (At::Sidebar { tab } | At::Placeholder { tab }, Visit::Out) => {
                if self.came_left(tab, stands) && tabs > 1 {
                    Then::Back
                } else {
                    Then::Beside
                }
            },
        };
        self.visit = Visit::Passing;
        sent
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
        /// A click on the sidebar: the end of the hover over it, or the click
        /// itself, through to it.
        Click,
        /// A key, anywhere.
        Key,
        Wait(u64),
        /// The pane beside the column in this tab closes.
        Close(usize),
    }

    use Step::{Ask, Click, Close, Key, See, Wait};
    use Then::{Back, Beside, Press, Stay};

    /// Takes in every step, in two tabs, and says what the sidebar did after
    /// each report.
    fn walk(steps: Vec<Step>) -> Vec<Then> {
        let mut pass = Pass::default();
        let mut frame = 0;
        let mut closed = Vec::new();
        let mut seen = Vec::new();
        for step in steps {
            match step {
                See(at) => seen.push(pass.see(at, 2, frame, |pane| !closed.contains(&pane))),
                Ask => pass.ask(frame),
                Click => pass.point(frame),
                Key => pass.input(),
                Wait(ticks) => frame += ticks,
                Close(tab) => closed.push(neighbour(tab)),
            }
        }
        seen
    }

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

    /// Every report the sidebar takes in, in two tabs, and what it does after
    /// each.
    #[test]
    fn the_focus_passes_over_the_column_unless_you_asked_for_it() {
        let cases = [
            (
                "right, into the next tab: its placeholder, then the sidebar in its place",
                vec![pane(1, true), placeholder(2), sidebar(2), pane(2, true)],
                vec![Stay, Beside, Stay, Stay],
            ),
            (
                "left, from the pane beside the column",
                vec![pane(2, true), sidebar(2), pane(1, false)],
                vec![Stay, Back, Stay],
            ),
            (
                "a click on the sidebar, from the pane beside it",
                vec![pane(2, true), Click, sidebar(2), sidebar(2)],
                vec![Stay, Press, Stay],
            ),
            (
                "the mouse on the sidebar does not keep a key that came from another tab",
                vec![pane(1, true), Click, sidebar(2)],
                vec![Stay, Beside],
            ),
            (
                "the pointer leaving the sidebar does not keep a key that comes later",
                vec![pane(2, true), Click, Wait(CLICK + 1), sidebar(2)],
                vec![Stay, Back],
            ),
            (
                "a key just as the hover ends keeps the focus a click may have brought",
                vec![pane(2, true), Click, Key, sidebar(2)],
                vec![Stay, Stay],
            ),
            (
                "in the sidebar, with floating panes shown and hidden again",
                vec![pane(2, true), Ask, sidebar(2), floating(), sidebar(2)],
                vec![Stay, Stay, Stay, Stay],
            ),
            (
                "on to a tab with floating panes shown, and hidden again over its column",
                vec![pane(2, true), sidebar(2), floating(), placeholder(1)],
                vec![Stay, Back, Stay, Beside],
            ),
            (
                "focus, through panes from before it came",
                vec![pane(2, true), Ask, pane(2, true), sidebar(2), sidebar(2), pane(2, true)],
                vec![Stay, Stay, Stay, Stay, Stay],
            ),
            (
                "focus that never came, and a key long after",
                vec![pane(2, true), Ask, Wait(WAIT + 1), sidebar(2)],
                vec![Stay, Back],
            ),
            (
                "in the sidebar, then on a placeholder, after a jump to another tab",
                vec![pane(2, true), Ask, sidebar(2), placeholder(1)],
                vec![Stay, Stay, Beside],
            ),
            (
                "out of the sidebar, then left into it again",
                vec![pane(2, true), Ask, sidebar(2), pane(2, true), sidebar(2)],
                vec![Stay, Stay, Stay, Back],
            ),
            (
                "from a pane that does not stand beside the column",
                vec![pane(2, false), sidebar(2)],
                vec![Stay, Beside],
            ),
            (
                "the pane beside the column closes, and zellij hands the sidebar the focus",
                vec![pane(2, true), Close(2), sidebar(2)],
                vec![Stay, Beside],
            ),
            (
                "in the sidebar as it leaves for another client's tab, on the placeholder it leaves",
                vec![pane(2, true), Ask, sidebar(2), placeholder(2)],
                vec![Stay, Stay, Beside],
            ),
        ];
        for (case, steps, sent) in cases {
            assert_eq!(walk(steps), sent, "{case}");
        }
    }

    /// The report that a click focuses the sidebar by says to act on it, once.
    /// One that `focus` brought does not, whatever the pointer did, nor one
    /// that a key since the hover's end may have brought.
    #[test]
    fn a_report_says_when_a_click_took_the_focus_in() {
        let cases = [
            ("a click", vec![pane(2, true), Click, sidebar(2)], Press),
            ("the report after it", vec![pane(2, true), Click, sidebar(2), sidebar(2)], Stay),
            ("focus, and a click", vec![pane(2, true), Ask, Click, sidebar(2)], Stay),
            ("a key, then a click", vec![pane(2, true), Key, Click, sidebar(2)], Press),
            ("a click, then a key", vec![pane(2, true), Click, Key, sidebar(2)], Stay),
        ];
        for (case, steps, then) in cases {
            assert_eq!(walk(steps).last(), Some(&then), "{case}");
        }
    }

    #[test]
    fn left_in_the_only_tab_goes_back_beside() {
        let mut pass = Pass::default();
        let beside = At::Pane { tab: TabId(1), beside: Some(neighbour(1)) };
        assert_eq!(pass.see(beside, 1, 0, |_| true), Stay);
        assert_eq!(pass.see(At::Sidebar { tab: TabId(1) }, 1, 0, |_| true), Beside);
    }
}
