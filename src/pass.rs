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
//! hover over the sidebar first, so a click comes as a mouse event just before
//! the panes that show the sidebar focused. A key comes with none.

use crate::TICKS_PER_SECOND;

/// How long the focus you asked for may take to come, in ticks. A hidden
/// sidebar comes back before the dock moves the focus into it.
const WAIT: u64 = 2 * TICKS_PER_SECOND;

/// Where this client's focus is, in one report of the panes and the tabs, a
/// tab by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// A pane of its own; `beside` when it stands right of the column.
    Pane { tab: usize, beside: bool },
    Sidebar { tab: usize },
    Placeholder { tab: usize },
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
    /// The pane the focus was on last, outside the column: its tab, and
    /// whether it stands beside the column.
    from: Option<(usize, bool)>,
    /// Whether the mouse was on the sidebar since the report before.
    pointed: bool,
}

impl Pass {
    pub fn ask(&mut self, frame: u64) {
        if self.visit != Visit::In {
            self.visit = Visit::Asked(frame);
        }
    }

    pub fn point(&mut self) {
        self.pointed = true;
    }

    /// Whether the focus is in the column only on its way somewhere else.
    pub fn passing(&self) -> bool {
        self.visit == Visit::Passing
    }

    /// Takes in where the focus is after a report, and says where to send it
    /// on, if anywhere. A focus sent on is not sent on again, whatever the
    /// reports that come before it gets there.
    pub fn see(&mut self, at: At, tabs: usize, frame: u64) -> Option<On> {
        let pointed = std::mem::take(&mut self.pointed);
        if let Visit::Asked(asked) = self.visit {
            if frame.wrapping_sub(asked) > WAIT {
                self.visit = Visit::Out;
            }
        }
        let tab = match (at, self.visit) {
            (At::Pane { tab, beside }, visit) => {
                self.from = Some((tab, beside));
                if !matches!(visit, Visit::Asked(_)) {
                    self.visit = Visit::Out;
                }
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
            (At::Sidebar { .. } | At::Placeholder { .. }, Visit::Passing)
            | (At::Placeholder { .. }, Visit::Asked(_)) => return None,
            (At::Sidebar { tab } | At::Placeholder { tab }, Visit::Out | Visit::In) => tab,
        };
        self.visit = Visit::Passing;
        let back = self.from == Some((tab, true)) && tabs > 1;
        Some(if back { On::Back } else { On::Beside })
    }

    fn came_within(&self, tab: usize) -> bool {
        self.from.is_some_and(|(from, _)| from == tab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Step {
        See(At),
        Ask,
        Point,
        Wait(u64),
    }

    use Step::{Ask, Point, See, Wait};

    const fn pane(tab: usize, beside: bool) -> Step {
        See(At::Pane { tab, beside })
    }

    const fn sidebar(tab: usize) -> Step {
        See(At::Sidebar { tab })
    }

    const fn placeholder(tab: usize) -> Step {
        See(At::Placeholder { tab })
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
                vec![pane(2, true), Point, sidebar(2), sidebar(2)],
                vec![None, None, None],
            ),
            (
                "the mouse on the sidebar does not keep a key that came from another tab",
                vec![pane(1, true), Point, sidebar(2)],
                vec![None, Some(On::Beside)],
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
        ];
        for (case, steps, sent) in cases {
            let mut pass = Pass::default();
            let mut frame = 0;
            let mut seen = Vec::new();
            for step in steps {
                match step {
                    See(at) => seen.push(pass.see(at, 2, frame)),
                    Ask => pass.ask(frame),
                    Point => pass.point(),
                    Wait(ticks) => frame += ticks,
                }
            }
            assert_eq!(seen, sent, "{case}");
        }
    }

    #[test]
    fn left_in_the_only_tab_goes_back_beside() {
        let mut pass = Pass::default();
        assert_eq!(pass.see(At::Pane { tab: 1, beside: true }, 1, 0), None);
        assert_eq!(pass.see(At::Sidebar { tab: 1 }, 1, 0), Some(On::Beside));
    }
}
