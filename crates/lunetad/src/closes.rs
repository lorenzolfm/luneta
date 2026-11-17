use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::api::{close_request, closes_response, kill_request, CloseRequest, On};
use crate::mux::{tabs, Asks, Laid, Mux, OwnKey, PaneId, SessionName, SocketFolder, TabId};
use crate::mux::Zellij;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closes {
    Pane,
    Tab { id: TabId, panes: Vec<u32> },
    Session,
}

pub fn closes_now(session: &SessionName, pane: u32) -> Result<Closes, String> {
    closing(&Mux::Zellij(session).laid()?, pane)
}

pub(crate) fn closing(laid: &[Laid], pane: u32) -> Result<Closes, String> {
    match together(laid, &BTreeSet::from([pane]))? {
        Closing::Session => Ok(Closes::Session),
        Closing::Parts { mut tabs, .. } => {
            Ok(tabs.pop().map_or(Closes::Pane, |(id, panes)| Closes::Tab { id, panes }))
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closing {
    Parts { tabs: Vec<(TabId, Vec<u32>)>, panes: Vec<u32> },
    Session,
}

pub fn closes_together(session: &SessionName, panes: &BTreeSet<u32>) -> Result<Closing, String> {
    together(&Mux::Zellij(session).laid()?, panes)
}

fn together(laid: &[Laid], panes: &BTreeSet<u32>) -> Result<Closing, String> {
    let given = |laid: &Laid| laid.pane.terminal().filter(|id| panes.contains(id));
    let listed = |pane: u32| laid.iter().any(|laid| laid.pane == PaneId::Terminal(pane));
    if let Some(gone) = panes.iter().find(|pane| !listed(**pane)) {
        return Err(format!("pane {gone} is gone"));
    }
    let tabs = tabs(laid);
    let in_tab = |tab: TabId| laid.iter().filter(move |laid| laid.tab_id == tab);
    let touched = tabs.iter().copied().filter(|tab| in_tab(*tab).any(|it| given(it).is_some()));
    let held = |tab: &TabId| in_tab(*tab).any(|laid| given(laid).is_none() && laid.holds_tab());
    let (held, closed): (Vec<TabId>, Vec<TabId>) = touched.partition(held);
    if closed.len() == tabs.len() {
        return Ok(Closing::Session);
    }
    let tabs = closed.into_iter().map(|tab| (tab, terminals(laid, tab))).collect();
    let alone = |tab| in_tab(tab).filter_map(given);
    let panes = held.into_iter().flat_map(alone).collect();
    Ok(Closing::Parts { tabs, panes })
}

pub(crate) fn terminals(laid: &[Laid], tab: TabId) -> Vec<u32> {
    let terminals = laid.iter().filter(|laid| laid.tab_id == tab);
    terminals.filter_map(|laid| laid.pane.terminal()).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Close {
    Pane(u32),
    Tab { id: TabId, panes: Vec<u32> },
}

impl Close {
    #[must_use]
    pub fn tab(laid: &[Laid], id: TabId) -> Self {
        Close::Tab { id, panes: terminals(laid, id) }
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>, session: &SessionName, whose: Whose) -> CloseRequest {
        let closes = match self {
            Close::Pane(pane) => close_request::Closes::Pane(*pane),
            Close::Tab { id, panes } => {
                close_request::Closes::Tab(closes_response::Tab { id: id.0, panes: panes.clone() })
            },
        };
        let session = session.to_string();
        CloseRequest { on, session, closes: Some(closes), whose: Some(whose.into()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Whose {
    Zellij(SystemTime),
    Own(OwnKey),
}

impl Whose {
    fn started(at: impl TryInto<SystemTime>) -> Result<Whose, String> {
        let unread = |_| "started_at: it says no time the session started at".to_owned();
        at.try_into().map(Whose::Zellij).map_err(unread)
    }

    fn own(own: &crate::api::OwnSession) -> Result<Whose, String> {
        OwnKey::of(own).map(Whose::Own)
    }

    pub fn closed(self, session: &SessionName, close: &Close) -> Result<(), String> {
        match self {
            Whose::Zellij(started) => closed(session, started, close),
            Whose::Own(_) => Err(no_own(session)),
        }
    }

    pub fn killed(self, session: &SessionName) -> Result<(), String> {
        match self {
            Whose::Zellij(started) => killed(session, started),
            Whose::Own(_) => Err(no_own(session)),
        }
    }
}

impl TryFrom<close_request::Whose> for Whose {
    type Error = String;

    fn try_from(whose: close_request::Whose) -> Result<Whose, String> {
        match whose {
            close_request::Whose::StartedAt(at) => Whose::started(at),
            close_request::Whose::Own(own) => Whose::own(&own),
        }
    }
}

impl TryFrom<kill_request::Whose> for Whose {
    type Error = String;

    fn try_from(whose: kill_request::Whose) -> Result<Whose, String> {
        match whose {
            kill_request::Whose::StartedAt(at) => Whose::started(at),
            kill_request::Whose::Own(own) => Whose::own(&own),
        }
    }
}

impl From<Whose> for close_request::Whose {
    fn from(whose: Whose) -> Self {
        match whose {
            Whose::Zellij(started) => close_request::Whose::StartedAt(started.into()),
            Whose::Own(key) => close_request::Whose::Own(key.into()),
        }
    }
}

impl From<Whose> for kill_request::Whose {
    fn from(whose: Whose) -> Self {
        match whose {
            Whose::Zellij(started) => kill_request::Whose::StartedAt(started.into()),
            Whose::Own(key) => kill_request::Whose::Own(key.into()),
        }
    }
}


pub fn closed(session: &SessionName, started: SystemTime, close: &Close) -> Result<(), String> {
    same_session(session, started)?;
    closed_as_listed(Mux::Zellij(session), close)
}

fn no_own(session: &SessionName) -> String {
    format!("\"{session}\" is no session luneta-mux holds, as it holds terminals alone")
}

pub(crate) fn same_session(session: &SessionName, started: SystemTime) -> Result<(), String> {
    let running = running_in(&Zellij::socket_folder())?;
    if running.iter().any(|(name, at)| name == session && *at != started) {
        return Err(format!("\"{session}\" is a session made since, which stays"));
    }
    Ok(())
}

pub(crate) fn closed_as_listed(mux: Mux<'_>, close: &Close) -> Result<(), String> {
    still(&mux.laid()?, close)?;
    match close {
        Close::Pane(pane) => mux.pane_closed(PaneId::Terminal(*pane), Asks::Once)?,
        Close::Tab { id, .. } => mux.tab_closed(*id, Asks::Once)?,
    }
    Ok(())
}

fn still(laid: &[Laid], close: &Close) -> Result<(), String> {
    match close {
        Close::Pane(pane) => match closing(laid, *pane)? {
            Closes::Pane => Ok(()),
            Closes::Tab { .. } | Closes::Session => {
                Err(format!("pane {pane} would take its tab along now, so it stays"))
            },
        },
        Close::Tab { id, panes } => {
            let now: BTreeSet<u32> = terminals(laid, *id).into_iter().collect();
            if now != panes.iter().copied().collect() {
                return Err(format!("tab {id} holds other panes than it did, so it stays"));
            }
            if tabs(laid).iter().all(|tab| tab == id) {
                return Err(format!("tab {id} would take the session along now, so it stays"));
            }
            Ok(())
        },
    }
}

const GOING: Duration = Duration::from_secs(1);

const GOING_LOOK: Duration = Duration::from_millis(10);

const SETTLING: Duration = Duration::from_millis(100);

pub fn killed(session: &SessionName, started: SystemTime) -> Result<(), String> {
    same_session(session, started)?;
    let folder = Zellij::socket_folder();
    Mux::Zellij(session).session_killed()?;
    settled(session, started, &folder)
}

fn settled(
    session: &SessionName,
    started: SystemTime,
    folder: &SocketFolder,
) -> Result<(), String> {
    let deadline = Instant::now() + GOING;
    while running_in(folder)?.iter().any(|(name, at)| name == session && *at == started) {
        if Instant::now() >= deadline {
            return Err(format!("zellij did not kill \"{session}\""));
        }
        thread::sleep(GOING_LOOK);
    }
    thread::sleep(SETTLING);
    let unknown =
        |why| format!("luneta cannot tell whether zellij keeps \"{session}\" in its cache: {why}");
    let cached = Zellij::cached(session).map_err(unknown)?;
    if cached && running_in(folder)?.iter().all(|(name, _)| name != session) {
        return deleted(session);
    }
    Ok(())
}

pub fn running_in(folder: &SocketFolder) -> Result<Vec<(SessionName, SystemTime)>, String> {
    folder.running().ok_or_else(|| {
        let folder = folder.path().display();
        format!("luneta cannot read all of {folder}, where zellij keeps its sessions' sockets")
    })
}

pub fn deleted(session: &SessionName) -> Result<(), String> {
    Ok(Mux::Zellij(session).session_deleted()?)
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
    fn the_last_pane_that_holds_its_tab_open_closes_the_tab_or_the_session() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let bar = Laid { pane: PaneId::Plugin(1), is_selectable: false, ..laid(1, 0) };
        let plugin = Laid { pane: PaneId::Plugin(4), ..laid(4, 0) };
        let suspended = Laid { is_suppressed: true, ..laid(7, 0) };
        let other_tab = laid(9, 1);
        let tab = |panes: Vec<u32>| Ok(Closes::Tab { id: TabId(0), panes });
        for (panes, id, closes) in [
            (vec![laid(4, 0), bar.clone(), other_tab.clone()], 4, tab(vec![4])),
            (vec![laid(4, 0), floating.clone(), other_tab.clone()], 4, tab(vec![4, 5])),
            (vec![laid(4, 0), laid(6, 0), other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![laid(4, 0), floating.clone(), other_tab.clone()], 5, Ok(Closes::Pane)),
            (vec![laid(4, 0), plugin, other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![suspended, laid(4, 0), other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![laid(4, 0), floating, bar], 4, Ok(Closes::Session)),
            (vec![other_tab], 4, Err("pane 4 is gone".to_owned())),
        ] {
            assert_eq!(closing(&panes, id), closes, "pane {id}");
        }
    }

    #[test]
    fn panes_closed_together_close_the_tab_none_left_holds_open() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let tab = vec![laid(4, 0), laid(6, 0), floating, laid(7, 1), laid(8, 1)];
        let parts =
            |tabs: Vec<(TabId, Vec<u32>)>, panes: Vec<u32>| Ok(Closing::Parts { tabs, panes });
        for (given, closes) in [
            (vec![4, 6], parts(vec![(TabId(0), vec![4, 6, 5])], vec![])),
            (vec![4, 7], parts(vec![], vec![4, 7])),
            (vec![4, 6, 7], parts(vec![(TabId(0), vec![4, 6, 5])], vec![7])),
            (vec![4, 6, 7, 8], Ok(Closing::Session)),
            (vec![4, 9], Err("pane 9 is gone".to_owned())),
        ] {
            let given: BTreeSet<u32> = given.iter().copied().collect();
            assert_eq!(together(&tab, &given), closes, "panes {given:?}");
        }
    }

    #[test]
    fn a_close_goes_ahead_only_while_zellij_lists_what_was_planned() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let bar = Laid { pane: PaneId::Plugin(1), is_selectable: false, ..laid(1, 0) };
        let planned = vec![laid(4, 0), laid(6, 0), floating.clone(), bar, laid(9, 1)];
        let opened = [planned.clone(), vec![laid(7, 0)]].concat();
        let last = planned[..4].to_vec();
        let left = vec![laid(4, 0), floating, laid(9, 1)];
        let tab = || Close::Tab { id: TabId(0), panes: vec![6, 5, 4] };
        let other = || Err("tab 0 holds other panes than it did, so it stays".to_owned());
        let session = Err("tab 0 would take the session along now, so it stays".to_owned());
        let along = Err("pane 4 would take its tab along now, so it stays".to_owned());
        for (listed, close, still_so) in [
            (&planned, Close::Pane(4), Ok(())),
            (&planned, tab(), Ok(())),
            (&opened, tab(), other()),
            (&last, tab(), session),
            (&left, tab(), other()),
            (&left, Close::Pane(4), along),
            (&planned, Close::Pane(8), Err("pane 8 is gone".to_owned())),
        ] {
            assert_eq!(still(listed, &close), still_so, "{close:?}");
        }
    }
}
