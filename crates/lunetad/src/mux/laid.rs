//! A session's panes as its multiplexer lays them out, in luneta's own words:
//! what each pane is, in which tab, how it is placed there, and what runs in
//! it.

use std::collections::BTreeSet;
use std::fmt;

/// A pane of a session's, as much of it as says what it holds open, whether
/// a client coming there is on it, and what runs in it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "each is a way of its own a pane is placed")]
pub struct Laid {
    pub pane: PaneId,
    /// Whether you can go to it: a bar you cannot.
    pub is_selectable: bool,
    /// Whether another took its place, off the screen, till that one closes.
    pub is_suppressed: bool,
    pub is_floating: bool,
    /// Whether a client has the focus on it in its tab, or the last one had.
    pub is_focused: bool,
    /// Whether it is shown over the other panes of its tab. Only a turn goes by
    /// it, to show it so again, as a pane leaving the tab or coming into it ends
    /// that.
    pub is_fullscreen: bool,
    /// Whether what ran in it ended, and it is held open with what it wrote.
    pub is_held: bool,
    /// What its frame calls it, by which a turn finds a stand-in of its own.
    pub title: String,
    pub tab_id: TabId,
    /// Where its tab stands among the session's, from the first.
    pub tab_position: u32,
    /// What its tab is called, as you pick a tab for another's panes by it.
    pub tab_name: String,
    /// Where it works: none where the multiplexer did not say, which does not
    /// mean it works nowhere.
    pub cwd: Option<String>,
    /// What runs in its foreground: none where the multiplexer did not say,
    /// which does not mean nothing runs.
    pub command: Option<String>,
}

impl Laid {
    /// Whether it is tiled in its tab, for you to go to: a plugin's as well.
    #[must_use]
    pub fn is_tiled(&self) -> bool {
        self.is_selectable && !self.is_suppressed && !self.is_floating
    }

    /// Whether it keeps its tab open: a tiled pane you can go to, a plugin's
    /// as well, or a terminal another took the place of, which comes back once
    /// that one closes. A floating pane goes with the tab, and so does a bar,
    /// which you cannot go to.
    #[must_use]
    pub fn holds_tab(&self) -> bool {
        self.is_tiled() || (self.is_suppressed && self.pane.terminal().is_some())
    }
}

/// A pane of a session's. Terminals and plugins are numbered apart, so that a
/// session can have one of each by an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneId {
    Terminal(u32),
    Plugin(u32),
}

impl PaneId {
    /// The terminal it is, or none: it is a plugin's.
    #[must_use]
    pub fn terminal(self) -> Option<u32> {
        match self {
            PaneId::Terminal(id) => Some(id),
            PaneId::Plugin(_) => None,
        }
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaneId::Terminal(id) => write!(f, "terminal_{id}"),
            PaneId::Plugin(id) => write!(f, "plugin_{id}"),
        }
    }
}

/// A tab, by the id its session gave it, which is numbered apart from its
/// panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(pub u32);

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The terminal of the tab whose id is `tab` among `laid`, the first it
/// lists: where luneta goes in a tab it opened.
#[must_use]
pub fn terminal_of(laid: &[Laid], tab: TabId) -> Option<u32> {
    laid.iter().filter(|laid| laid.tab_id == tab).find_map(|laid| laid.pane.terminal())
}

/// The ids of the tabs `laid` lists a pane of, a plugin's as well.
#[must_use]
pub fn tabs(laid: &[Laid]) -> BTreeSet<TabId> {
    laid.iter().map(|pane| pane.tab_id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid(id: u32, tab_id: u32) -> Laid {
        Laid {
            pane: PaneId::Terminal(id),
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: false,
            is_fullscreen: false,
            is_held: false,
            title: String::new(),
            tab_id: TabId(tab_id),
            tab_position: tab_id,
            tab_name: String::new(),
            cwd: None,
            command: None,
        }
    }

    /// luneta goes to the terminal of a tab it opened, of that tab, past a
    /// plugin's pane there of the same id.
    #[test]
    fn a_tab_opened_is_gone_to_on_its_terminal() {
        let plugin = Laid { pane: PaneId::Plugin(3), ..laid(3, 1) };
        let panes = [laid(1, 0), plugin, laid(3, 1)];
        assert_eq!(terminal_of(&panes, TabId(1)), Some(3));
    }

    /// The tabs a session was made with, which a tab opened there closes, are
    /// every tab it lists, those that hold a plugin's pane alone as well.
    #[test]
    fn a_made_session_s_tabs_are_all_it_lists_a_plugin_s_too() {
        let bar = Laid { pane: PaneId::Plugin(0), ..laid(0, 2) };
        let panes = [laid(0, 0), laid(1, 1), laid(2, 1), bar];
        assert_eq!(tabs(&panes), BTreeSet::from([TabId(0), TabId(1), TabId(2)]));
    }
}
