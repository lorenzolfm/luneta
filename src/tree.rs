//! Every live session and the panes in it, as zellij lists them: what the
//! picker names an agent by, and what a delete key closes.

use zellij_tile::prelude::{PaneInfo, PaneManifest, TabInfo};

use crate::close::{self, Closes};
use crate::places::Places;

/// A tab, by the id zellij keeps for it while it lives. Its position changes
/// as the tabs before it close; the id does not. Only `of` makes one, so a
/// position never passes for an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(usize);

impl TabId {
    /// The id of `tab`, and not its position, which zellij hands over beside
    /// it as a number of the same kind.
    pub fn of(tab: &TabInfo) -> Self {
        TabId(tab.tab_id)
    }

    /// The id, as zellij takes a tab by it.
    pub fn id(self) -> usize {
        self.0
    }
}

/// The terminal you are in, of a tab's panes: the focused tiled one, or else
/// the focused floating one. The picker and the menu float over the tab with
/// the focus, and are no terminal.
pub fn origin(panes: &[PaneInfo]) -> Option<u32> {
    let focused = |floating: bool| {
        panes.iter().find(move |p| {
            p.is_focused && !p.is_plugin && !p.is_suppressed && p.is_floating == floating
        })
    };
    focused(false).or_else(|| focused(true)).map(|p| p.id)
}

/// A live session and its panes.
pub struct Branch {
    pub name: String,
    pub panes: Vec<Leaf>,
}

/// A terminal pane, the name of the tab that holds it and the id zellij keeps
/// for that tab, and the directory it works in, as its session last said.
pub struct Leaf {
    pub id: u32,
    pub title: String,
    pub tab: String,
    pub tab_id: Option<TabId>,
    pub cwd: Option<String>,
    /// What closing it closes with it: its tab once nothing else holds the
    /// tab open, and its session once that is the session's only tab.
    pub closes: Closes,
}

impl Branch {
    /// `before` is the session's panes as the picker last had them.
    pub fn new(
        name: String,
        tabs: &[TabInfo],
        manifest: &PaneManifest,
        places: &Places,
        before: &[Leaf],
    ) -> Self {
        let panes = leaves(&name, tabs, manifest, places, before);
        Branch { name, panes }
    }
}

/// The panes a person can go to, in tab order, the tiled ones of a tab before
/// its floating ones, and in the order they were opened within each. A
/// session that leaves a pane out of one answer has not said it moved, and
/// the pane keeps the directory it had `before`.
pub fn leaves(
    session: &str,
    tabs: &[TabInfo],
    manifest: &PaneManifest,
    places: &Places,
    before: &[Leaf],
) -> Vec<Leaf> {
    let mut keyed: Vec<((usize, bool, u32), Leaf)> = Vec::new();
    for (position, panes) in &manifest.panes {
        let tab = tabs.iter().find(|tab| tab.position == *position);
        let tab_id = tab.map(TabId::of);
        let tab = tab.map_or("", |tab| tab.name.as_str());
        for pane in panes {
            if pane.is_plugin || !pane.is_selectable || pane.is_suppressed {
                continue;
            }
            let held = before.iter().find(|leaf| leaf.id == pane.id);
            let cwd = places
                .cwd(session, pane.id)
                .or_else(|| held.and_then(|leaf| leaf.cwd.as_deref()));
            keyed.push((
                (*position, pane.is_floating, pane.id),
                Leaf {
                    id: pane.id,
                    title: pane.title.clone(),
                    tab: tab.to_owned(),
                    tab_id,
                    cwd: cwd.map(str::to_owned),
                    closes: close::closes(panes, pane.id, manifest.panes.len()),
                },
            ));
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, leaf)| leaf).collect()
}

#[cfg(test)]
mod tests {
    use zellij_tile::prelude::PaneInfo;

    use super::*;

    fn pane(id: u32, title: &str) -> PaneInfo {
        PaneInfo { id, title: title.to_owned(), is_selectable: true, ..PaneInfo::default() }
    }

    fn tab(position: usize, name: &str) -> TabInfo {
        TabInfo { position, name: name.to_owned(), ..TabInfo::default() }
    }

    fn manifest(tabs: &[(usize, Vec<PaneInfo>)]) -> PaneManifest {
        PaneManifest { panes: tabs.iter().cloned().collect() }
    }

    #[test]
    fn panes_come_in_tab_order_and_only_the_ones_you_can_go_to() {
        let plugin = PaneInfo { is_plugin: true, ..pane(0, "zellij:tab-bar") };
        let hidden = PaneInfo { is_suppressed: true, ..pane(9, "hidden") };
        let floating = PaneInfo { is_floating: true, ..pane(1, "floating") };
        let bar = PaneInfo { is_selectable: false, ..pane(8, "bar") };
        let manifest = manifest(&[
            (1, vec![pane(2, "second tab")]),
            (0, vec![floating, plugin, pane(5, "tiled, later"), pane(3, "tiled"), hidden, bar]),
        ]);
        let places = Places::of(&[("s", &[(3, "/w/s"), (2, "/w/logs")]), ("t", &[(5, "/w/t")])]);
        let tabs = [tab(0, "code"), tab(1, "logs")];
        let leaves = leaves("s", &tabs, &manifest, &places, &[]);
        let seen: Vec<(u32, &str, &str, Option<&str>)> = leaves
            .iter()
            .map(|leaf| (leaf.id, leaf.title.as_str(), leaf.tab.as_str(), leaf.cwd.as_deref()))
            .collect();
        // Pane 5's directory is session `t`'s, not this one's.
        assert_eq!(
            seen,
            [
                (3, "tiled", "code", Some("/w/s")),
                (5, "tiled, later", "code", None),
                (1, "floating", "code", None),
                (2, "second tab", "logs", Some("/w/logs")),
            ]
        );
    }

    /// A pane says what closing it closes: itself beside another, its tab
    /// alone in it, and its session alone in the session's only tab.
    #[test]
    fn a_pane_says_what_closing_it_closes() {
        let two = manifest(&[(0, vec![pane(2, "a"), pane(3, "b")]), (1, vec![pane(4, "c")])]);
        let one = manifest(&[(0, vec![pane(4, "c")])]);
        let places = Places::default();
        let closes = |manifest: &PaneManifest, tabs: &[TabInfo]| {
            let leaves = leaves("s", tabs, manifest, &places, &[]);
            leaves.iter().map(|leaf| (leaf.id, leaf.closes)).collect::<Vec<_>>()
        };
        let tabs = [tab(0, "code"), tab(1, "logs")];
        let of_two = [(2, Closes::Pane), (3, Closes::Pane), (4, Closes::Tab)];
        assert_eq!(closes(&two, &tabs), of_two);
        assert_eq!(closes(&one, &tabs[..1]), [(4, Closes::Session)]);
    }

    /// A pane its session left out of an answer keeps its directory, and one
    /// its session says moved takes the new one.
    #[test]
    fn a_pane_keeps_its_directory_until_its_session_says_where_it_works_now() {
        let manifest = manifest(&[(0, vec![pane(1, "fish"), pane(2, "fish")])]);
        let tabs = [tab(0, "code")];
        let places = Places::of(&[("s", &[(1, "/w/repo"), (2, "/w/repo")])]);
        let before = leaves("s", &tabs, &manifest, &places, &[]);

        let places = Places::of(&[("s", &[(1, "/w/repo/src")])]);
        let after = leaves("s", &tabs, &manifest, &places, &before);
        let cwds: Vec<Option<&str>> = after.iter().map(|leaf| leaf.cwd.as_deref()).collect();
        assert_eq!(cwds, [Some("/w/repo/src"), Some("/w/repo")]);
    }
}
