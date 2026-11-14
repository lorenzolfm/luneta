use std::collections::BTreeSet;
use std::fmt;

use luneta_protocol::said::Said;

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "each is a way of its own a pane is placed")]
pub struct Laid {
    pub pane: PaneId,
    pub is_selectable: bool,
    pub is_suppressed: bool,
    pub is_floating: bool,
    pub is_focused: bool,
    pub is_fullscreen: bool,
    pub is_held: bool,
    pub title: String,
    pub tab_id: TabId,
    pub tab_position: u32,
    pub tab_name: String,
    pub cwd: Option<Said>,
    pub command: Option<Said>,
}

impl Laid {
    #[must_use]
    pub fn is_tiled(&self) -> bool {
        self.is_selectable && !self.is_suppressed && !self.is_floating
    }

    #[must_use]
    pub fn holds_tab(&self) -> bool {
        self.is_tiled() || (self.is_suppressed && self.pane.terminal().is_some())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneId {
    Terminal(u32),
    Plugin(u32),
}

impl PaneId {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(pub u32);

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[must_use]
pub fn terminal_of(laid: &[Laid], tab: TabId) -> Option<u32> {
    laid.iter().filter(|laid| laid.tab_id == tab).find_map(|laid| laid.pane.terminal())
}

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

    #[test]
    fn a_tab_opened_is_gone_to_on_its_terminal() {
        let plugin = Laid { pane: PaneId::Plugin(3), ..laid(3, 1) };
        let panes = [laid(1, 0), plugin, laid(3, 1)];
        assert_eq!(terminal_of(&panes, TabId(1)), Some(3));
    }

    #[test]
    fn a_made_session_s_tabs_are_all_it_lists_a_plugin_s_too() {
        let bar = Laid { pane: PaneId::Plugin(0), ..laid(0, 2) };
        let panes = [laid(0, 0), laid(1, 1), laid(2, 1), bar];
        assert_eq!(tabs(&panes), BTreeSet::from([TabId(0), TabId(1), TabId(2)]));
    }
}
