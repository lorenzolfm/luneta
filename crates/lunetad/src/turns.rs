use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::tab;

use crate::closes::{same_session, terminals};
use crate::mux::{Asks, Client, Laid, Listed, Mux, PaneId, SessionName, TabId};

const STAND_IN: [&str; 2] = ["sleep", "10"];

const STAND_IN_NAME: &str = "luneta stand-in";

const SECOND_STAND_IN_NAME: &str = "luneta second stand-in";

const FINDING: Duration = Duration::from_millis(100);
const LOOKING: Duration = Duration::from_millis(5);

const NOWHERE: PaneId = PaneId::Terminal(u32::MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    PaneIntoTab,
    TabIntoPanes { beside: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntoTab {
    Done,
    NoClient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntoPanes {
    Done,
    Partly { moved: u32, of: u32, why: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherTab {
    pub pane: u32,
    pub name: String,
}

pub fn other_tabs(session: &SessionName, pane: u32) -> Result<Vec<OtherTab>, String> {
    others(&Mux::Zellij(session).laid()?, pane)
}

fn others(laid: &[Laid], pane: u32) -> Result<Vec<OtherTab>, String> {
    let own = terminal(laid, pane)?.tab_id;
    let others = laid.iter().filter(|it| it.tab_id != own);
    let mut tabs: Vec<(&Laid, u32)> =
        others.filter_map(|it| Some((it, it.pane.terminal()?))).collect();
    tabs.sort_by_key(|(it, _)| it.tab_position);
    tabs.dedup_by_key(|(it, _)| it.tab_id);
    Ok(tabs.into_iter().map(|(it, pane)| OtherTab { pane, name: it.tab_name.clone() }).collect())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Leaving {
    pane: u32,
    name: Option<String>,
    fullscreen: Option<PaneId>,
}

impl Leaving {
    #[must_use]
    pub fn pane(&self) -> PaneId {
        PaneId::Terminal(self.pane)
    }

    #[must_use]
    pub fn fullscreen(&self) -> Option<PaneId> {
        self.fullscreen
    }

    pub fn turn(&self, session: &SessionName) -> Result<(), String> {
        let tab = new_tab(session, self.name.as_deref())?;
        let laid = Mux::Zellij(session).laid()?;
        let stand_ins: Vec<PaneId> =
            terminals(&laid, tab).into_iter().map(PaneId::Terminal).collect();
        let moved = self.pane();
        let listed = over_stand_ins(session, &stand_ins, moved, tab)?;
        match landed(&listed, moved, tab) {
            Landed::There => Ok(()),
            Landed::Stayed => Err(no_room(moved, tab)),
            Landed::Gone => Err(format!("pane {} is gone", self.pane)),
        }
    }
}

pub fn pane_leaving(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
) -> Result<Option<Leaving>, String> {
    same_session(session, started)?;
    let laid = Mux::Zellij(session).laid()?;
    let name = leaving(&laid, pane)?;
    let fullscreen = fullscreen_of(&laid, terminal(&laid, pane)?.tab_id)
        .filter(|it| *it != PaneId::Terminal(pane));
    Ok(has_client(session)?.then_some(Leaving { pane, name, fullscreen }))
}

pub fn into_tab(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
) -> Result<IntoTab, String> {
    match pane_leaving(session, started, pane)? {
        Some(leaving) => leaving.turn(session).map(|()| IntoTab::Done),
        None => Ok(IntoTab::NoClient),
    }
}

fn leaving(laid: &[Laid], pane: u32) -> Result<Option<String>, String> {
    let leaves = terminal(laid, pane)?;
    let beside = |it: &&Laid| {
        it.tab_id == leaves.tab_id && it.pane != PaneId::Terminal(pane) && !suppressed_plugin(it)
    };
    let others: Vec<&Laid> = laid.iter().filter(beside).collect();
    if others.is_empty() {
        return Err(format!("pane {pane} is alone in its tab already"));
    }
    if !others.iter().any(|it| it.holds_tab()) {
        return Err(format!(
            "pane {pane} alone holds its tab open, whose floating panes would end with it, so it \
             stays"
        ));
    }
    Ok(tab::named(&leaves.tab_name).map(str::to_owned))
}

pub fn panes_moving(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
    beside: u32,
) -> Result<Moving, String> {
    same_session(session, started)?;
    moving(&Mux::Zellij(session).laid()?, pane, beside)
}

pub fn into_panes(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
    beside: u32,
) -> Result<IntoPanes, String> {
    panes_moving(session, started, pane, beside)?.turn(session)
}

fn stopped(moved: u32, of: u32, why: String) -> Result<IntoPanes, String> {
    if moved == 0 {
        return Err(why);
    }
    Ok(IntoPanes::Partly { moved, of, why })
}

#[derive(Debug, PartialEq, Eq)]
pub struct Moving {
    from: TabId,
    to: TabId,
    panes: Vec<PaneId>,
    fullscreen: Option<PaneId>,
}

impl Moving {
    #[must_use]
    pub fn panes(&self) -> &[PaneId] {
        &self.panes
    }

    #[must_use]
    pub fn fullscreen(&self) -> Option<PaneId> {
        self.fullscreen
    }

    pub fn turn(&self, session: &SessionName) -> Result<IntoPanes, String> {
        let mut of = u32::try_from(self.panes.len()).unwrap_or(u32::MAX);
        let mut moved = 0;
        for &it in &self.panes {
            match over_stand_in(session, it, self.to) {
                Ok(Landed::There) => moved += 1,
                Ok(Landed::Gone) => of -= 1,
                Ok(Landed::Stayed) => return stopped(moved, of, no_room(it, self.to)),
                Err(why) => return stopped(moved, of, why),
            }
        }
        match close_emptied(session, self.from) {
            Ok(()) => Ok(IntoPanes::Done),
            Err(why) => stopped(moved, of, why),
        }
    }
}

fn moving(laid: &[Laid], pane: u32, beside: u32) -> Result<Moving, String> {
    let from = terminal(laid, pane)?.tab_id;
    let to = terminal(laid, beside)
        .map_err(|_| format!("pane {beside}, which the tab goes by, is gone"))?
        .tab_id;
    if from == to {
        return Err(format!("pane {pane} is in the tab of pane {beside} already"));
    }
    let moves = |it: &&Laid| it.tab_id == from && !suppressed_plugin(it);
    let (tiled, others): (Vec<&Laid>, Vec<&Laid>) =
        laid.iter().filter(moves).partition(|it| it.is_tiled());
    let panes = others.into_iter().chain(tiled).map(|it| it.pane).collect();
    Ok(Moving { from, to, panes, fullscreen: fullscreen_of(laid, to) })
}

fn suppressed_plugin(it: &Laid) -> bool {
    it.is_suppressed && matches!(it.pane, PaneId::Plugin(_))
}

fn fullscreen_of(laid: &[Laid], tab: TabId) -> Option<PaneId> {
    laid.iter().find(|it| it.tab_id == tab && it.is_fullscreen).map(|it| it.pane)
}

pub fn fullscreen_again(session: &SessionName, pane: PaneId) -> Result<(), String> {
    if !ended_fullscreen(&Mux::Zellij(session).laid()?, &clients_of(session)?, pane) {
        return Ok(());
    }
    Ok(Mux::Zellij(session).fullscreen_toggled(pane)?)
}

fn ended_fullscreen(laid: &[Laid], clients: &[Client], pane: PaneId) -> bool {
    let Some(shown) = laid.iter().find(|it| it.pane == pane) else {
        return false;
    };
    let beside = |client: &Client| {
        client.pane != pane
            && laid.iter().any(|it| it.tab_id == shown.tab_id && it.pane == client.pane)
    };
    !shown.is_fullscreen && !clients.iter().any(beside)
}

fn terminal(laid: &[Laid], pane: u32) -> Result<&Laid, String> {
    let found = laid.iter().find(|it| it.pane == PaneId::Terminal(pane));
    found.ok_or_else(|| format!("pane {pane} is gone"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landed {
    There,
    Stayed,
    Gone,
}

fn landed(laid: &[Laid], pane: PaneId, tab: TabId) -> Landed {
    match laid.iter().find(|it| it.pane == pane) {
        Some(it) if it.tab_id == tab => Landed::There,
        Some(_) => Landed::Stayed,
        None => Landed::Gone,
    }
}

fn no_room(pane: PaneId, tab: TabId) -> String {
    format!("zellij found no room for {pane} in tab {tab}, so it stays")
}

fn has_client(session: &SessionName) -> Result<bool, String> {
    Ok(Mux::Zellij(session).has_clients()?)
}

fn clients_of(session: &SessionName) -> Result<Listed, String> {
    Ok(Mux::Zellij(session).clients()?)
}

fn new_tab(session: &SessionName, name: Option<&str>) -> Result<TabId, String> {
    Ok(Mux::Zellij(session).tab_opened_once_heard(name, &STAND_IN)?)
}

fn over_stand_in(session: &SessionName, pane: PaneId, to: TabId) -> Result<Landed, String> {
    let stand_in = open_stand_in(session, to, STAND_IN_NAME)?;
    let listed = over_stand_ins(session, &[stand_in], pane, to)?;
    Ok(landed(&listed, pane, to))
}

fn open_stand_in(session: &SessionName, to: TabId, name: &str) -> Result<PaneId, String> {
    let found = || found_stand_in(session, to, name);
    Ok(PaneId::Terminal(Mux::Zellij(session).pane_opened_once_heard(to, name, &STAND_IN, found)?))
}

fn found_stand_in(session: &SessionName, to: TabId, name: &str) -> Option<u32> {
    let deadline = Instant::now() + FINDING;
    loop {
        let listed = Mux::Zellij(session).laid().unwrap_or_default();
        let is_it = |it: &&Laid| it.tab_id == to && it.title == name;
        if let Some(id) = listed.iter().filter(is_it).filter_map(|it| it.pane.terminal()).max() {
            return Some(id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(LOOKING);
    }
}

fn close_stand_in(session: &SessionName, pane: PaneId) {
    let _ = Mux::Zellij(session).pane_closed(pane, Asks::OnceReached);
}

fn over_stand_ins(
    session: &SessionName,
    stand_ins: &[PaneId],
    moved: PaneId,
    tab: TabId,
) -> Result<Vec<Laid>, String> {
    let Some(&root) = stand_ins.first() else {
        return Err("zellij opened a tab with no pane".to_owned());
    };
    let came = stacked(session, root, moved, tab);
    let last = match came {
        Ok(Stacked { unfocused: true, .. }) => under_stack(session, root, tab),
        _ => None,
    };
    for &stand_in in stand_ins {
        close_stand_in(session, stand_in);
    }
    if let Some(last) = last {
        close_stand_in(session, last);
    }
    came.map(|came| came.listed)
}

struct Stacked {
    listed: Vec<Laid>,
    unfocused: bool,
}

fn stacked(
    session: &SessionName,
    root: PaneId,
    moved: PaneId,
    tab: TabId,
) -> Result<Stacked, String> {
    let on_it = |clients: Listed| clients.iter().any(|it| it.pane == moved);
    if !clients_of(session).is_ok_and(on_it) {
        stack(session, [root, moved, NOWHERE])?;
        let listed = Mux::Zellij(session).laid()?;
        match landed(&listed, moved, tab) {
            Landed::There => return Ok(Stacked { listed, unfocused: true }),
            Landed::Gone => return Ok(Stacked { listed, unfocused: false }),
            Landed::Stayed => {},
        }
    }
    stack(session, [root, moved])?;
    Ok(Stacked { listed: Mux::Zellij(session).laid()?, unfocused: false })
}

fn under_stack(session: &SessionName, root: PaneId, tab: TabId) -> Option<PaneId> {
    let last = open_stand_in(session, tab, SECOND_STAND_IN_NAME).ok()?;
    let _ = stack(session, [root, last, NOWHERE]);
    Some(last)
}

fn stack<const N: usize>(session: &SessionName, panes: [PaneId; N]) -> Result<(), String> {
    Ok(Mux::Zellij(session).panes_stacked(panes)?)
}

fn close_emptied(session: &SessionName, from: TabId) -> Result<(), String> {
    if has_client(session)? || Mux::Zellij(session).laid()?.iter().any(|it| it.tab_id == from) {
        return Ok(());
    }
    Ok(Mux::Zellij(session).tab_closed(from, Asks::OnceReached)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32, tab_id: u32) -> Laid {
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
            tab_name: format!("Tab #{}", tab_id + 1),
            cwd: None,
            command: None,
        }
    }

    fn floating(laid: Laid) -> Laid {
        Laid { is_floating: true, ..laid }
    }

    fn suppressed(laid: Laid) -> Laid {
        Laid { is_suppressed: true, ..laid }
    }

    fn plugin(laid: Laid) -> Laid {
        let PaneId::Terminal(id) = laid.pane else {
            panic!("a plugin of a terminal's pane");
        };
        Laid { pane: PaneId::Plugin(id), ..laid }
    }

    #[test]
    fn a_pane_leaves_a_tab_another_holds_open_with_its_name() {
        let named = |laid: Laid| Laid { tab_name: "auth".to_owned(), ..laid };
        assert_eq!(leaving(&[pane(1, 0), pane(2, 0)], 1), Ok(None));
        assert_eq!(leaving(&[floating(pane(1, 0)), pane(2, 0)], 1), Ok(None));
        let auth = Ok(Some("auth".to_owned()));
        assert_eq!(leaving(&[named(pane(1, 0)), named(pane(2, 0))], 1), auth);
        let parked = [pane(1, 0), suppressed(pane(2, 0))];
        assert_eq!(leaving(&parked, 1), Ok(None));
    }

    #[test]
    fn a_pane_alone_in_its_tab_stays() {
        let alone = "pane 1 is alone in its tab already";
        assert_eq!(leaving(&[pane(1, 0), pane(2, 1)], 1), Err(alone.to_owned()));
        let hidden = [pane(1, 0), suppressed(plugin(pane(2, 0)))];
        assert_eq!(leaving(&hidden, 1), Err(alone.to_owned()));
    }

    #[test]
    fn a_pane_that_alone_holds_its_tab_open_stays_for_the_floating_ones() {
        let why = leaving(&[pane(1, 0), floating(pane(2, 0))], 1).unwrap_err();
        assert!(why.contains("floating panes would end"), "{why}");
    }

    #[test]
    fn a_pane_that_is_gone_turns_into_nothing() {
        let gone = "pane 9 is gone".to_owned();
        assert_eq!(leaving(&[pane(1, 0)], 9), Err(gone.clone()));
        assert_eq!(moving(&[pane(1, 0)], 9, 1), Err(gone.clone()));
        assert_eq!(others(&[pane(1, 0)], 9), Err(gone));
        assert!(leaving(&[plugin(pane(9, 0)), pane(1, 0)], 9).is_err());
    }

    #[test]
    fn the_tiled_panes_of_a_tab_go_last() {
        let laid = [
            pane(1, 0),
            floating(pane(2, 0)),
            pane(3, 0),
            suppressed(pane(4, 0)),
            plugin(pane(5, 0)),
            suppressed(plugin(pane(6, 0))),
            pane(7, 1),
            pane(8, 2),
        ];
        let Moving { from, panes, .. } = moving(&laid, 3, 7).unwrap();
        assert_eq!(from, TabId(0));
        let spelled = panes.iter().map(ToString::to_string).collect::<Vec<_>>();
        let order = ["terminal_2", "terminal_4", "terminal_1", "terminal_3", "plugin_5"];
        assert_eq!(spelled, order);
    }

    #[test]
    fn panes_go_to_the_tab_of_the_terminal_picked() {
        let laid = [pane(1, 0), pane(2, 0), plugin(pane(7, 1)), pane(7, 5)];
        assert_eq!(moving(&laid, 1, 7).unwrap().to, TabId(5));
    }

    #[test]
    fn a_tab_turns_into_panes_of_another_tab_only() {
        let laid = [pane(1, 0), pane(3, 0), pane(2, 1)];
        assert_eq!(moving(&laid, 1, 3), Err("pane 1 is in the tab of pane 3 already".to_owned()));
        let gone = "pane 7, which the tab goes by, is gone".to_owned();
        assert_eq!(moving(&laid, 1, 7), Err(gone));
    }

    #[test]
    fn the_other_tabs_go_in_their_order() {
        let at = |laid: Laid, tab_position| Laid { tab_position, ..laid };
        let laid = [
            at(pane(1, 4), 2),
            pane(2, 0),
            at(pane(3, 7), 0),
            at(pane(4, 7), 0),
            at(pane(5, 2), 1),
            at(floating(pane(6, 9)), 3),
            at(plugin(pane(8, 8)), 4),
        ];
        let tabs = others(&laid, 2).unwrap();
        let panes: Vec<u32> = tabs.iter().map(|tab| tab.pane).collect();
        assert_eq!(panes, [3, 5, 1, 6]);
        assert_eq!(tabs[0].name, "Tab #8");
        assert_eq!(others(&[pane(1, 0)], 1), Ok(Vec::new()));
    }

    #[test]
    fn what_zellij_lists_says_where_a_pane_landed() {
        let moved = PaneId::Terminal(1);
        let to = TabId(3);
        assert_eq!(landed(&[pane(1, 3)], moved, to), Landed::There);
        assert_eq!(landed(&[pane(1, 0)], moved, to), Landed::Stayed);
        assert_eq!(landed(&[pane(2, 3)], moved, to), Landed::Gone);
        assert_eq!(landed(&[plugin(pane(1, 3))], moved, to), Landed::Gone);
    }
}
