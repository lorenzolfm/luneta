//! What zellij moves of a session of the machine it runs on, for a key of
//! luneta's menu or a command of its CLI: a pane into a tab of its own, or the
//! panes of a tab into another tab, which the tab they left goes with. What runs
//! in them runs on, and each keeps its id. luneta has zellij move them on its
//! own machine; a daemon has it move them here for a luneta on another machine,
//! whose menu was for a row of this one.
//!
//! zellij's CLI has no command for it, but its `stack-panes` takes a pane out
//! of whatever tab it is in, by its id, to stack it on another
//! (`stack_panes` in zellij-server 0.45.1's `screen.rs`). So a pane moves by
//! stacking on a stand-in opened where it goes, which then closes and leaves
//! it tiled in its place. One at a time, the stack asks the least room of
//! zellij. Where it has none, it takes nothing and says nothing, so each move
//! is read back from what zellij lists.
//!
//! No pane floats on the way, though floating is what takes one out of a
//! stack: zellij's toggle takes a pane out of the tiled ones before it looks
//! for room to float it, and with none, as in a window of under 15 rows or
//! columns, it drops the pane, says nothing, and what runs in it runs on with
//! no pane to reach it by (`add_floating_pane` in zellij-server's `tab`).
//!
//! A tab goes by a terminal in it, from the moment it is listed to the moment
//! it is picked: zellij gives the id of a tab that closed to the next it
//! opens, and never a terminal's while its server runs.
//!
//! zellij moves the client that last typed in the session to the last pane it is
//! told to stack, and hides the floating panes of the tab it goes to. It passes
//! over a pane that is none of the session's, so with `NOWHERE` last it moves no
//! client, as `stacked` has it: what you type goes on reaching your pane. A client
//! on the pane that moves is to go with it, so that one goes with no `NOWHERE`,
//! as does one with no room for it, which counts as a pane of the stack. zellij
//! then does move the client that last typed: the menu takes luneta's own there in
//! the end, and the CLI takes that client back. A pane that moves with no client
//! taken to it needs a second stand-in for it to end a pane like any other, as
//! `under_stack` says. A tab whose last tiled pane leaves closes as zellij next
//! draws for a client, with the floating panes left in it, which end: so those go
//! first, and a pane that alone holds its tab open stays where they would end.
//!
//! zellij makes a tab with no room for its panes in a session no client is in,
//! so only a pane that would move says it needs one: one that stays moves no
//! client there to find that out.
//!
//! A session goes by when it started as well as its name: zellij numbers panes
//! from 0 for each server, so one that took the name since has panes of the
//! same ids.
//!
//! Closing a stand-in can cut short the calls that follow it, as `cli::heard`
//! says, so those are asked again, but for the `new-pane` of a stand-in, which
//! opens all the same, and is found by its name: asked again, it would open a second
//! that stands in the tab until it ends on its own. A `stack-panes` cut short can still be on
//! its way as zellij lists the panes, and read as having no room: the pane then
//! stays, or moves after all, and nothing is lost either way, as what goes then
//! is its stand-in, and never a tab, which would end a pane late to land there.
//! A client it takes along goes after the CLI has taken it back, and no one says.
//!
//! zellij's CLI holds no condition on what it last listed. So a tiled pane
//! that ends between the listing and the move can still have its tab close on
//! the floating panes, and a tab that closes in that moment can give its id to
//! another. Nothing here can tell: only reading zellij afresh keeps that moment
//! as short as it is.
//!
//! A turn reads what it would move before it asks zellij to move anything, in
//! `pane_leaving` or `panes_moving`, so that what stops it there has moved nobody,
//! and `Leaving` and `Moving` say what it asks zellij to move.

use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::tab;

use crate::cli::{self, laid, Asks, Laid, PaneId, TabId};
use crate::closes::{same_session, terminals};
use crate::zellij::SessionName;

/// What runs in a pane opened for another until that one is in its place: what
/// ends on its own, which closes the pane, and a tab with nothing else in it,
/// where luneta is gone before it does.
const STAND_IN: [&str; 2] = ["sleep", "10"];

/// What a stand-in is named, for `open_stand_in` to find it.
const STAND_IN_NAME: &str = "luneta stand-in";

/// What the second stand-in of `under_stack` is named: found by the first's name,
/// it would be the first.
const SECOND_STAND_IN_NAME: &str = "luneta second stand-in";

/// How long a stand-in whose answer was cut short has to show in what zellij lists,
/// and how often it is looked for: it opens a moment after.
const FINDING: Duration = Duration::from_millis(100);
const LOOKING: Duration = Duration::from_millis(5);

/// What zellij is told to stack last for it to move no client, as it focuses the
/// last pane it is told to stack: a pane none of a session's has, as zellij
/// numbers terminals from 0 and gives no id of one again while its server runs.
const NOWHERE: PaneId = PaneId::Terminal(u32::MAX);

/// What a pane of a session turns into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// A tab of its own, named as the tab it leaves is, if someone named that
    /// one. The tab it leaves keeps its other panes.
    PaneIntoTab,
    /// Panes of the tab of the terminal `beside`, with the other panes of its
    /// tab, which goes.
    TabIntoPanes { beside: u32 },
}

/// What came of asking zellij to turn a pane into a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntoTab {
    /// It did.
    Done,
    /// Nothing moved: a session no client is in has no room for a tab, as
    /// `opens::no_client` says, and a client that comes there makes it.
    NoClient,
}

/// What came of asking zellij to turn a tab into panes of another, where any
/// moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntoPanes {
    /// All that were there moved, and the tab they left goes.
    Done,
    /// `moved` of the `of` that were there, less any that ended on the way,
    /// did, tiled where they went, and the rest stay in their tab, for `why`.
    Partly { moved: u32, of: u32, why: String },
}

/// A tab the panes of another can turn into: a terminal in it, which names it,
/// and its name, as zellij lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherTab {
    pub pane: u32,
    pub name: String,
}

/// The tabs of `session` other than that of its terminal `pane`, in their
/// order, as zellij lists its panes now, or why it does not say.
pub fn other_tabs(session: &SessionName, pane: u32) -> Result<Vec<OtherTab>, String> {
    others(&laid(session)?, pane)
}

/// The tabs among `laid` other than that of the terminal `pane`, in their
/// order, that have a terminal to name them by; or why not: it is gone.
fn others(laid: &[Laid], pane: u32) -> Result<Vec<OtherTab>, String> {
    let own = terminal(laid, pane)?.tab_id;
    let mut tabs: Vec<&Laid> = laid.iter().filter(|it| !it.is_plugin && it.tab_id != own).collect();
    tabs.sort_by_key(|it| it.tab_position);
    tabs.dedup_by_key(|it| it.tab_id);
    Ok(tabs.into_iter().map(|it| OtherTab { pane: it.id, name: it.tab_name.clone() }).collect())
}

/// A pane that can turn into a tab of its own, and what that tab is named, as
/// zellij lists them: it has been asked to move nothing yet.
#[derive(Debug, PartialEq, Eq)]
pub struct Leaving {
    pane: u32,
    name: Option<String>,
    fullscreen: Option<PaneId>,
}

impl Leaving {
    /// The pane that turns.
    #[must_use]
    pub fn pane(&self) -> PaneId {
        PaneId::Terminal(self.pane)
    }

    /// The pane shown fullscreen in the tab it leaves, which its leaving ends.
    #[must_use]
    pub fn fullscreen(&self) -> Option<PaneId> {
        self.fullscreen
    }

    /// Moves it into a tab made for it in `session`, or says why it did not,
    /// where it stays where it was, and the tab goes as what holds it closes.
    /// What runs in it runs on either way.
    pub fn turn(&self, session: &SessionName) -> Result<(), String> {
        let tab = new_tab(session, self.name.as_deref())?;
        let stand_ins: Vec<PaneId> =
            terminals(&laid(session)?, tab).into_iter().map(PaneId::Terminal).collect();
        let moved = self.pane();
        let listed = over_stand_ins(session, &stand_ins, moved, tab)?;
        match landed(&listed, moved, tab) {
            Landed::There => Ok(()),
            Landed::Stayed => Err(no_room(moved, tab)),
            Landed::Gone => Err(format!("pane {} is gone", self.pane)),
        }
    }
}

/// What turning the terminal `pane` of `session`, the one that started at
/// `started`, into a tab would move, as zellij lists it now, with nothing moved;
/// or why nothing would. None where no client is in the session, which has no
/// room for the tab: a pane that stays says so ahead of that.
pub fn pane_leaving(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
) -> Result<Option<Leaving>, String> {
    same_session(session, started)?;
    let laid = laid(session)?;
    let name = leaving(&laid, pane)?;
    let fullscreen = fullscreen_of(&laid, terminal(&laid, pane)?.tab_id)
        .filter(|it| *it != PaneId::Terminal(pane));
    Ok(has_client(session)?.then_some(Leaving { pane, name, fullscreen }))
}

/// Moves the terminal `pane` of `session`, the one that started at `started`,
/// into a tab made for it, as `pane_leaving` and `Leaving::turn` say, and says
/// what came of it, or why it did not.
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

/// What the tab made for the terminal `pane` among `laid`, the panes of its
/// session, is named: as the tab it leaves is, if someone named that one. Or
/// why it does not leave: it is gone, or nothing would hold that tab open
/// after it, as its floating panes do not, which would end.
fn leaving(laid: &[Laid], pane: u32) -> Result<Option<String>, String> {
    let leaves = terminal(laid, pane)?;
    let is_it = |it: &&Laid| !it.is_plugin && it.id == pane;
    let beside = |it: &&Laid| {
        it.tab_id == leaves.tab_id && !is_it(it) && !(it.is_suppressed && it.is_plugin)
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

/// What moving the panes of the tab of the terminal `pane` of `session`, the one
/// that started at `started`, into the tab of the terminal `beside` would move,
/// as zellij lists it now, with nothing moved; or why nothing would.
pub fn panes_moving(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
    beside: u32,
) -> Result<Moving, String> {
    same_session(session, started)?;
    moving(&laid(session)?, pane, beside)
}

/// Moves the panes of the tab of the terminal `pane` of `session`, the one that
/// started at `started`, into the tab of the terminal `beside`, as `panes_moving`
/// and `Moving::turn` say, and says what came of it where any moved, or why none
/// did.
pub fn into_panes(
    session: &SessionName,
    started: SystemTime,
    pane: u32,
    beside: u32,
) -> Result<IntoPanes, String> {
    panes_moving(session, started, pane, beside)?.turn(session)
}

/// What came of zellij stopping for `why` once `moved` of `of` panes had: none
/// moved, or some did.
fn stopped(moved: u32, of: u32, why: String) -> Result<IntoPanes, String> {
    if moved == 0 {
        return Err(why);
    }
    Ok(IntoPanes::Partly { moved, of, why })
}

/// What moving the panes of a tab into another moves: the tab they leave, the
/// tab they go into, and the panes, in the order they go. zellij has been asked
/// to move none of them yet.
#[derive(Debug, PartialEq, Eq)]
pub struct Moving {
    from: TabId,
    to: TabId,
    panes: Vec<PaneId>,
    fullscreen: Option<PaneId>,
}

impl Moving {
    /// The panes it moves, in the order they go.
    #[must_use]
    pub fn panes(&self) -> &[PaneId] {
        &self.panes
    }

    /// The pane shown fullscreen in the tab they go into, which their coming
    /// ends.
    #[must_use]
    pub fn fullscreen(&self) -> Option<PaneId> {
        self.fullscreen
    }

    /// Moves them in `session`, tiled, and the tab they left goes; and says what
    /// came of it where any moved, or why none did. One that ends on its way does
    /// not stop the rest: what runs in it had ended anyway.
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

/// What moving the panes of the tab of the terminal `pane` into the tab of the
/// terminal `beside` moves, among `laid`, the panes of their session: all of
/// them, the tiled ones last, as the floating ones end once the last tiled one
/// has left. A suppressed plugin's goes with the tab. Or why it moves nothing:
/// either terminal is gone, or both are in one tab.
fn moving(laid: &[Laid], pane: u32, beside: u32) -> Result<Moving, String> {
    let from = terminal(laid, pane)?.tab_id;
    let to = terminal(laid, beside)
        .map_err(|_| format!("pane {beside}, which the tab goes by, is gone"))?
        .tab_id;
    if from == to {
        return Err(format!("pane {pane} is in the tab of pane {beside} already"));
    }
    let moves = |it: &&Laid| it.tab_id == from && !(it.is_suppressed && it.is_plugin);
    let (tiled, others): (Vec<&Laid>, Vec<&Laid>) =
        laid.iter().filter(moves).partition(|it| it.is_tiled());
    let panes = others.into_iter().chain(tiled).map(Laid::pane_id).collect();
    Ok(Moving { from, to, panes, fullscreen: fullscreen_of(laid, to) })
}

/// The pane shown fullscreen in the tab `tab` among `laid`, if one is.
fn fullscreen_of(laid: &[Laid], tab: TabId) -> Option<PaneId> {
    laid.iter().find(|it| it.tab_id == tab && it.is_fullscreen).map(Laid::pane_id)
}

/// Shows `pane` of `session` fullscreen again, as it was before a turn, once the
/// turn is over: a pane that leaves its tab, or a stand-in that opens there, ends
/// that. Not one that is gone, or that still is, nor while a client is on another
/// pane of its tab, whom zellij would move to it.
pub fn fullscreen_again(session: &SessionName, pane: PaneId) -> Result<(), String> {
    if !ended_fullscreen(&laid(session)?, &clients_of(session)?, pane) {
        return Ok(());
    }
    Ok(cli::fullscreen_toggled(session, pane)?)
}

/// Whether `pane`, among `laid`, the panes of a session that `clients` are in, is
/// no longer shown fullscreen and can be again with no client moved.
fn ended_fullscreen(laid: &[Laid], clients: &[cli::Client], pane: PaneId) -> bool {
    let Some(shown) = laid.iter().find(|it| it.pane_id() == pane) else {
        return false;
    };
    let beside = |client: &cli::Client| {
        client.pane != pane
            && laid.iter().any(|it| it.tab_id == shown.tab_id && it.pane_id() == client.pane)
    };
    !shown.is_fullscreen && !clients.iter().any(beside)
}

/// The terminal `pane` among `laid`, or why not: it is gone.
fn terminal(laid: &[Laid], pane: u32) -> Result<&Laid, String> {
    let found = laid.iter().find(|it| !it.is_plugin && it.id == pane);
    found.ok_or_else(|| format!("pane {pane} is gone"))
}

/// Where a pane is that zellij was asked to move into a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landed {
    /// In that tab.
    There,
    /// Elsewhere: zellij says nothing of a stack it has no room for, and
    /// stacks nothing.
    Stayed,
    /// Nowhere zellij lists: what ran in it ended, which it says nothing of
    /// either.
    Gone,
}

/// Where `laid`, the panes of a session, lists `pane`, as it was to move into
/// the tab `tab`.
fn landed(laid: &[Laid], pane: PaneId, tab: TabId) -> Landed {
    match laid.iter().find(|it| it.pane_id() == pane) {
        Some(it) if it.tab_id == tab => Landed::There,
        Some(_) => Landed::Stayed,
        None => Landed::Gone,
    }
}

/// Why `pane` stays out of the tab `tab`.
fn no_room(pane: PaneId, tab: TabId) -> String {
    format!("zellij found no room for {pane} in tab {tab}, so it stays")
}

/// Whether a client is in `session`, as zellij lists them now.
fn has_client(session: &SessionName) -> Result<bool, String> {
    Ok(cli::has_clients(session)?)
}

/// The clients in `session`, as zellij lists them now.
fn clients_of(session: &SessionName) -> Result<cli::Listed, String> {
    Ok(cli::clients(session)?)
}

/// Has zellij open a tab for a pane in `session`, named `name` or as zellij
/// numbers it, with no client moved, and says its id. One zellij opened as a
/// pane closing cut its answer short has a second open beside it, and ends on
/// its own with its stand-in.
fn new_tab(session: &SessionName, name: Option<&str>) -> Result<TabId, String> {
    Ok(cli::tab_opened_once_heard(session, name, &STAND_IN)?)
}

/// Moves `pane` to the tab `to` of `session`, by stacking it on a stand-in
/// opened there, which then closes and leaves it in its place; and says where
/// it ended up, or why zellij was not asked to the end.
fn over_stand_in(session: &SessionName, pane: PaneId, to: TabId) -> Result<Landed, String> {
    let stand_in = open_stand_in(session, to, STAND_IN_NAME)?;
    let listed = over_stand_ins(session, &[stand_in], pane, to)?;
    Ok(landed(&listed, pane, to))
}

/// Has zellij open a stand-in named `name` in the tab `to` of `session`, with no
/// client moved, and says which. One whose answer a pane closing cut short opens
/// all the same, so it is found by its name, and asked for again only where it does
/// not show: that one has a second open beside it, which ends on its own.
fn open_stand_in(session: &SessionName, to: TabId, name: &str) -> Result<PaneId, String> {
    let found = || found_stand_in(session, to, name);
    Ok(PaneId::Terminal(cli::pane_opened_once_heard(session, to, name, &STAND_IN, found)?))
}

/// The newest stand-in named `name` in the tab `to` of `session`, as zellij lists
/// it within `FINDING`, or none.
fn found_stand_in(session: &SessionName, to: TabId, name: &str) -> Option<u32> {
    let deadline = Instant::now() + FINDING;
    loop {
        let listed = laid(session).unwrap_or_default();
        let is_it = |it: &&Laid| !it.is_plugin && it.tab_id == to && it.title == name;
        if let Some(it) = listed.iter().filter(is_it).max_by_key(|it| it.id) {
            return Some(it.id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(LOOKING);
    }
}

/// Has zellij close the stand-in `pane` of `session`. What came of the move
/// stands whether it does: a stand-in ends on its own, and closes as it does.
fn close_stand_in(session: &SessionName, pane: PaneId) {
    let _ = cli::pane_closed(session, pane, Asks::OnceReached);
}

/// Has zellij stack `moved` on the first of `stand_ins`, in the tab `tab` of
/// `session`, closes them, and says what it listed with `moved` stacked. `moved`
/// is left a pane like any other, as `under_stack` says.
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

/// What came of having zellij stack a pane: what it lists then, and whether it
/// stacked it with `NOWHERE` last, so that it moved no client.
struct Stacked {
    listed: Vec<Laid>,
    unfocused: bool,
}

/// Has zellij stack `moved` on `root`, in the tab `tab` of `session`, and says what
/// came of it. With `NOWHERE` it moves no client, unless one is on `moved`, as that
/// one goes with it. A stack with no room does nothing, and `NOWHERE` takes up room:
/// asked again without it, it moves the pane where it ever did, and the client that
/// last typed along with it.
fn stacked(
    session: &SessionName,
    root: PaneId,
    moved: PaneId,
    tab: TabId,
) -> Result<Stacked, String> {
    let on_it = |clients: cli::Listed| clients.iter().any(|it| it.pane == moved);
    if !clients_of(session).is_ok_and(on_it) {
        stack(session, [root, moved, NOWHERE])?;
        let listed = laid(session)?;
        match landed(&listed, moved, tab) {
            Landed::There => return Ok(Stacked { listed, unfocused: true }),
            Landed::Gone => return Ok(Stacked { listed, unfocused: false }),
            Landed::Stayed => {},
        }
    }
    stack(session, [root, moved])?;
    Ok(Stacked { listed: laid(session)?, unfocused: false })
}

/// Has zellij stack a second stand-in in the tab `tab` of `session` under the pane
/// stacked on `root` with no client moved, and says which, to close after `root`.
/// In a tab no client is on, that pane has all the room of the stack and `root` a
/// row of it. Closing a pane of one row leaves the other a stack of one, which
/// zellij splits for no stand-in after it, and a new pane opened on it joins. It
/// makes a pane like any other only of closing the one with the room, and the
/// second stand-in takes that from it. Where it finds no room for this one, the
/// pane stays a stack of one, as it ends with no client in the session: it has
/// moved all the same.
fn under_stack(session: &SessionName, root: PaneId, tab: TabId) -> Option<PaneId> {
    let last = open_stand_in(session, tab, SECOND_STAND_IN_NAME).ok()?;
    let _ = stack(session, [root, last, NOWHERE]);
    Some(last)
}

/// Has zellij stack `panes` on the first of them, each from whatever tab it is
/// in. It stacks nothing where it has no room, and says nothing of it.
fn stack<const N: usize>(session: &SessionName, panes: [PaneId; N]) -> Result<(), String> {
    Ok(cli::panes_stacked(session, panes)?)
}

/// Closes the tab `from`, which the panes of `session` left, where zellij
/// keeps it: it closes one as it next draws for a client, and with none it
/// lists it on with nothing in it. One that has panes in it by now stays.
fn close_emptied(session: &SessionName, from: TabId) -> Result<(), String> {
    if has_client(session)? || laid(session)?.iter().any(|it| it.tab_id == from) {
        return Ok(());
    }
    Ok(cli::tab_closed(session, from, Asks::OnceReached)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32, tab_id: u32) -> Laid {
        Laid {
            id,
            is_plugin: false,
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: false,
            is_fullscreen: false,
            title: String::new(),
            tab_id: TabId(tab_id),
            tab_position: tab_id,
            tab_name: format!("Tab #{}", tab_id + 1),
            pane_command: None,
        }
    }

    fn floating(laid: Laid) -> Laid {
        Laid { is_floating: true, ..laid }
    }

    fn suppressed(laid: Laid) -> Laid {
        Laid { is_suppressed: true, ..laid }
    }

    fn plugin(laid: Laid) -> Laid {
        Laid { is_plugin: true, ..laid }
    }

    /// A pane leaves a tab another pane holds open, whether it floats itself,
    /// and takes the name of the tab where someone named it.
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

    /// A pane alone in its tab has nothing to do, whatever else its session
    /// has, and a plugin's pane suppressed there changes nothing.
    #[test]
    fn a_pane_alone_in_its_tab_stays() {
        let alone = "pane 1 is alone in its tab already";
        assert_eq!(leaving(&[pane(1, 0), pane(2, 1)], 1), Err(alone.to_owned()));
        let hidden = [pane(1, 0), suppressed(plugin(pane(2, 0)))];
        assert_eq!(leaving(&hidden, 1), Err(alone.to_owned()));
    }

    /// zellij closes a tab with its last tiled pane, and the floating panes
    /// in it end: one that alone holds its tab open stays, and says why.
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
        // A plugin's pane of that id is no terminal.
        assert!(leaving(&[plugin(pane(9, 0)), pane(1, 0)], 9).is_err());
    }

    /// The floating panes of a tab, and those another stands in for, go ahead
    /// of the tiled ones, whose last closes the tab; a plugin's goes too, but
    /// for one zellij keeps suppressed, which goes with the tab.
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

    /// zellij gives the id of a tab that closed to the next it opens: panes go
    /// to the tab the terminal picked is in now, whichever tab holds that id
    /// by then, and a plugin's pane of that id is no terminal.
    #[test]
    fn panes_go_to_the_tab_of_the_terminal_picked() {
        let laid = [pane(1, 0), pane(2, 0), plugin(pane(7, 1)), pane(7, 5)];
        assert_eq!(moving(&laid, 1, 7).unwrap().to, TabId(5));
    }

    /// The terminal picked may have closed, and taken its tab, since; nothing
    /// moves to whatever took that tab's id then.
    #[test]
    fn a_tab_turns_into_panes_of_another_tab_only() {
        let laid = [pane(1, 0), pane(3, 0), pane(2, 1)];
        assert_eq!(moving(&laid, 1, 3), Err("pane 1 is in the tab of pane 3 already".to_owned()));
        let gone = "pane 7, which the tab goes by, is gone".to_owned();
        assert_eq!(moving(&laid, 1, 7), Err(gone));
    }

    /// The other tabs go in their order, each once, as zellij lists a tab of
    /// each of its panes and not by its id, each by a terminal in it; a tab
    /// with none has nothing to go by.
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

    /// zellij exits 0 on a stack it did not do, for no room or for a pane that
    /// ended: only what it lists tells them apart.
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
