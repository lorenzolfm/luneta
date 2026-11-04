//! What zellij moves of a session of the machine it runs on, for a key of
//! luneta's menu: a pane into a tab of its own, or the panes of a tab into
//! another tab, which the tab they left goes with. What runs in them runs on,
//! and each keeps its id. luneta has zellij move them on its own machine; a
//! daemon has it move them here for a luneta on another machine, whose menu
//! was for a row of this one.
//!
//! zellij's CLI has no command for it, but its `stack-panes` takes a pane out
//! of whatever tab it is in, by its id, to stack it on another
//! (`stack_panes` in zellij-server 0.45.1's `screen.rs`). So a pane turns into
//! a tab by stacking on what a tab made for it holds, which then closes. The
//! panes of a tab go into another each by stacking on a pane there and
//! floating, which takes it out of the stack; embedded once all are floating,
//! so that no stack is left to go back to, they are tiled as zellij tiles a
//! pane it opens. One at a time, the stack asks the least room of zellij.
//!
//! zellij moves the client that last typed in the session to each pane it
//! stacks, and luneta takes its own there in the end. A tab whose last tiled
//! pane leaves closes as zellij next draws for a client, with the floating
//! panes left in it, which end: so those go first, and a pane that alone holds
//! its tab open stays where they would end.
//!
//! zellij makes a tab with no room for its panes in a session no client is in,
//! so only a pane that would move says it needs one: one that stays moves no
//! client there to find that out.

use std::fmt;
use std::process::Command;

use luneta_protocol::tab;

use crate::api::{turn_request, On, TurnRequest};
use crate::cli::{answered, done, laid, quiet, Laid};
use crate::clients;
use crate::closes::terminals;
use crate::zellij::SessionName;

/// What runs in a tab made for a pane until the pane is in it: what ends on
/// its own, which closes the tab, where luneta is gone before it does.
const STAND_IN: [&str; 2] = ["sleep", "10"];

/// What a pane of a session turns into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// A tab of its own, named as the tab it leaves is, if someone named that
    /// one. The tab it leaves keeps its other panes.
    PaneIntoTab,
    /// Panes of the tab `to`, by its id, with the other panes of its tab,
    /// which goes.
    TabIntoPanes { to: u32 },
}

impl Turn {
    /// The question that has zellij turn the terminal `pane` of `session` so,
    /// on the machine `on` names, or on the daemon's own, where it names none.
    #[must_use]
    pub fn asked(self, on: Option<On>, session: &SessionName, pane: u32) -> TurnRequest {
        let becomes = match self {
            Turn::PaneIntoTab => turn_request::Becomes::NewTab(turn_request::NewTab {}),
            Turn::TabIntoPanes { to } => turn_request::Becomes::PanesOf(to),
        };
        TurnRequest { on, session: session.to_string(), pane, becomes: Some(becomes) }
    }
}

/// What came of asking zellij to turn a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turned {
    /// It did.
    Done,
    /// Nothing moved: a session no client is in has no room for a tab, as
    /// `opens::no_client` says, and a client that comes there makes it.
    NoClient,
}

/// A tab the panes of another can turn into: its id, and its name, as zellij
/// lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherTab {
    pub id: u32,
    pub name: String,
}

/// The tabs of `session` other than that of its terminal `pane`, in their
/// order, as zellij lists its panes now, or why it does not say.
pub fn other_tabs(session: &SessionName, pane: u32) -> Result<Vec<OtherTab>, String> {
    others(&laid(session.as_str())?, pane)
}

/// The tabs among `laid` other than that of the terminal `pane`, in their
/// order, that have a pane to stack others on; or why not: it is gone.
fn others(laid: &[Laid], pane: u32) -> Result<Vec<OtherTab>, String> {
    let own = terminal(laid, pane)?.tab_id;
    let mut tabs: Vec<&Laid> = laid.iter().filter(|it| it.tab_id != own).collect();
    tabs.sort_by_key(|it| it.tab_position);
    tabs.dedup_by_key(|it| it.tab_id);
    tabs.retain(|it| root_of(laid, it.tab_id).is_some());
    Ok(tabs.into_iter().map(|it| OtherTab { id: it.tab_id, name: it.tab_name.clone() }).collect())
}

/// Has zellij turn the terminal `pane` of `session` as `turn` says, and says
/// what came of it, or why it did not. What runs in it runs on either way.
pub fn turned(session: &SessionName, pane: u32, turn: Turn) -> Result<Turned, String> {
    match turn {
        Turn::PaneIntoTab => into_tab(session, pane),
        Turn::TabIntoPanes { to } => into_panes(session, pane, to).map(|()| Turned::Done),
    }
}

/// Moves the terminal `pane` of `session` into a tab made for it. A pane that
/// stays says so ahead of a session with no room for the tab. Where zellij
/// finds no room to stack it, the tab goes, and it stays where it was.
fn into_tab(session: &SessionName, pane: u32) -> Result<Turned, String> {
    let name = leaving(&laid(session.as_str())?, pane)?;
    if !has_client(session)? {
        return Ok(Turned::NoClient);
    }
    let tab = new_tab(session, name.as_deref())?;
    let stand_ins = terminals(&laid(session.as_str())?, tab);
    let Some(root) = stand_ins.first() else {
        return Err("zellij opened a tab with no pane".to_owned());
    };
    let moved = PaneId::terminal(pane);
    stack(session, PaneId::terminal(*root), moved)?;
    if !in_tab(&laid(session.as_str())?, moved, tab) {
        let mut close = action(session);
        close.args(["close-tab-by-id", &tab.to_string()]);
        done(close)?;
        return Err(no_room(moved, tab));
    }
    for stand_in in stand_ins {
        let mut close = action(session);
        close.args(["close-pane", "--pane-id", &PaneId::terminal(stand_in).to_string()]);
        done(close)?;
    }
    Ok(Turned::Done)
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

/// Moves the panes of the tab of the terminal `pane` of `session` into the
/// tab `to`, tiled, and the tab they left goes. What did move by the time
/// zellij fails stays there, tiled.
fn into_panes(session: &SessionName, pane: u32, to: u32) -> Result<(), String> {
    let Moving { from, root, panes } = moving(&laid(session.as_str())?, pane, to)?;
    let mut floated = Vec::with_capacity(panes.len());
    let mut went = Ok(());
    for moved in panes {
        went = float_over(session, root, moved, to);
        if went.is_err() {
            break;
        }
        floated.push(moved);
    }
    let embedded = floated.iter().try_for_each(|moved| float_or_embed(session, *moved));
    went.and(embedded)?;
    close_emptied(session, from)
}

/// What moving the panes of a tab into another moves: the tab they leave,
/// the pane of the other that they stack on, and the panes, in the order they
/// go.
#[derive(Debug, PartialEq, Eq)]
struct Moving {
    from: u32,
    root: PaneId,
    panes: Vec<PaneId>,
}

/// What moving the panes of the tab of the terminal `pane` into the tab `to`
/// moves, among `laid`, the panes of their session: all of them, the tiled
/// ones last, as the floating ones end once the last tiled one has left. A
/// suppressed plugin's goes with the tab. Or why it moves nothing: the
/// terminal is gone, or is in that tab, or that tab is gone.
fn moving(laid: &[Laid], pane: u32, to: u32) -> Result<Moving, String> {
    let from = terminal(laid, pane)?.tab_id;
    if from == to {
        return Err(format!("pane {pane} is in tab {to} already"));
    }
    let root = root_of(laid, to).ok_or_else(|| format!("tab {to} is gone"))?;
    let moves = |it: &&Laid| it.tab_id == from && !(it.is_suppressed && it.is_plugin);
    let (tiled, others): (Vec<&Laid>, Vec<&Laid>) =
        laid.iter().filter(moves).partition(|it| it.is_tiled());
    let panes = others.into_iter().chain(tiled).map(PaneId::of).collect();
    Ok(Moving { from, root, panes })
}

/// The pane of the tab `tab` among `laid` to stack others on: a tiled one,
/// as zellij stacks on no other, and its tallest, as zellij stacks only on a
/// pane tall enough.
fn root_of(laid: &[Laid], tab: u32) -> Option<PaneId> {
    let tiled = laid.iter().filter(|it| it.tab_id == tab && it.is_tiled());
    tiled.max_by_key(|it| it.pane_rows).map(PaneId::of)
}

/// The terminal `pane` among `laid`, or why not: it is gone.
fn terminal(laid: &[Laid], pane: u32) -> Result<&Laid, String> {
    let found = laid.iter().find(|it| !it.is_plugin && it.id == pane);
    found.ok_or_else(|| format!("pane {pane} is gone"))
}

/// Whether `laid`, the panes of a session, lists `pane` in the tab `tab`.
fn in_tab(laid: &[Laid], pane: PaneId, tab: u32) -> bool {
    laid.iter().any(|it| PaneId::of(it) == pane && it.tab_id == tab)
}

/// Why `pane` stays out of the tab `tab`: zellij says nothing of a stack it
/// has no room for, and stacks nothing.
fn no_room(pane: PaneId, tab: u32) -> String {
    format!("zellij found no room for {pane} in tab {tab}, so it stays")
}

/// Whether a client is in `session`, as zellij lists them now.
fn has_client(session: &SessionName) -> Result<bool, String> {
    let mut zellij = Command::new("zellij");
    quiet(&mut zellij);
    Ok(!clients::list(zellij, session.as_str())?.is_empty())
}

/// zellij's `action` command on `session`.
fn action(session: &SessionName) -> Command {
    let mut command = Command::new("zellij");
    command.args(["--session", session.as_str(), "action"]);
    command
}

/// Has zellij open a tab for a pane in `session`, named `name` or as zellij
/// numbers it, with no client moved, and says its id.
fn new_tab(session: &SessionName, name: Option<&str>) -> Result<u32, String> {
    let mut command = action(session);
    quiet(&mut command);
    command.args(["new-tab", "--no-focus", "--close-on-exit"]);
    command.args(name.iter().flat_map(|name| ["--name", name]));
    command.arg("--").args(STAND_IN);
    let said = answered(&mut command)?;
    said.trim().parse().map_err(|_| format!("zellij opened no tab it named: {:?}", said.trim()))
}

/// Has zellij stack `moved` on `root`, from whatever tab it is in. It stacks
/// nothing where it has no room, and says nothing of it.
fn stack(session: &SessionName, root: PaneId, moved: PaneId) -> Result<(), String> {
    let mut command = action(session);
    command.args(["stack-panes", "--"]).arg(root.to_string()).arg(moved.to_string());
    done(command)
}

/// Has zellij float `pane`, or embed it where it floats.
fn float_or_embed(session: &SessionName, pane: PaneId) -> Result<(), String> {
    let mut command = action(session);
    command.args(["toggle-pane-embed-or-floating", "--pane-id"]).arg(pane.to_string());
    done(command)
}

/// Moves `moved` to the tab `to`, by stacking it on `root` there, and floats
/// it there, or says why not. One zellij found no room for is not floated: it
/// would float in the tab it stays in.
fn float_over(session: &SessionName, root: PaneId, moved: PaneId, to: u32) -> Result<(), String> {
    stack(session, root, moved)?;
    if !in_tab(&laid(session.as_str())?, moved, to) {
        return Err(no_room(moved, to));
    }
    float_or_embed(session, moved)
}

/// Closes the tab `from`, which the panes of `session` left, where zellij
/// keeps it: it closes one as it next draws for a client, and with none it
/// lists it on with nothing in it. One that has panes in it by now stays.
fn close_emptied(session: &SessionName, from: u32) -> Result<(), String> {
    if has_client(session)? || laid(session.as_str())?.iter().any(|it| it.tab_id == from) {
        return Ok(());
    }
    let mut close = action(session);
    close.args(["close-tab-by-id", &from.to_string()]);
    done(close)
}

/// A pane as zellij's CLI takes it: a terminal's or a plugin's, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaneId {
    id: u32,
    is_plugin: bool,
}

impl PaneId {
    fn of(laid: &Laid) -> Self {
        PaneId { id: laid.id, is_plugin: laid.is_plugin }
    }

    fn terminal(id: u32) -> Self {
        PaneId { id, is_plugin: false }
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let PaneId { id, is_plugin } = self;
        if *is_plugin {
            write!(f, "plugin_{id}")
        } else {
            write!(f, "terminal_{id}")
        }
    }
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
            tab_id,
            tab_position: tab_id,
            tab_name: format!("Tab #{}", tab_id + 1),
            pane_rows: 30,
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
        assert_eq!(moving(&[pane(1, 0)], 9, 0), Err(gone.clone()));
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
        let Moving { from, panes, .. } = moving(&laid, 3, 1).unwrap();
        assert_eq!(from, 0);
        let spelled = panes.iter().map(ToString::to_string).collect::<Vec<_>>();
        let order = ["terminal_2", "terminal_4", "terminal_1", "terminal_3", "plugin_5"];
        assert_eq!(spelled, order);
    }

    /// Panes stack on the tallest tiled pane of the tab they go into, where
    /// zellij has the most room: it stacks on no floating one, nor on one
    /// another stands in for.
    #[test]
    fn panes_stack_on_the_tallest_tiled_pane_there() {
        let tall = |id, rows| Laid { pane_rows: rows, ..pane(id, 1) };
        let laid = [
            pane(1, 0),
            tall(2, 10),
            tall(3, 20),
            floating(tall(4, 50)),
            suppressed(tall(5, 60)),
        ];
        assert_eq!(moving(&laid, 1, 1).unwrap().root, PaneId::terminal(3));
    }

    #[test]
    fn a_tab_turns_into_panes_of_another_tab_only() {
        let laid = [pane(1, 0), pane(2, 1)];
        assert_eq!(moving(&laid, 1, 0), Err("pane 1 is in tab 0 already".to_owned()));
        assert_eq!(moving(&laid, 1, 7), Err("tab 7 is gone".to_owned()));
    }

    /// The other tabs go in their order, each once, as zellij lists a tab of
    /// each of its panes and not by its id; a tab with no tiled pane has none
    /// to stack on.
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
        ];
        let tabs = others(&laid, 2).unwrap();
        let ids: Vec<u32> = tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(ids, [7, 2, 4]);
        assert_eq!(tabs[0].name, "Tab #8");
        assert_eq!(others(&[pane(1, 0)], 1), Ok(Vec::new()));
    }
}
