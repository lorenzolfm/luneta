//! What zellij closes in a session of the machine it runs on, for a key of
//! luneta's menu or a command of its CLI: a pane, a tab, or the session,
//! which it kills and deletes; and what closing a pane closes with it, as
//! zellij closes it. luneta has zellij close it on its own machine; a daemon
//! has it close here for a luneta on another machine, whose menu was for a
//! row of this one.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::api::{close_request, closes_response, CloseRequest, On};
use crate::cli::{done, laid, quiet, tabs, unrun, Laid, ANSWER};
use crate::program;
use crate::zellij::SessionName;

/// What closing a terminal of a session closes with it, as zellij closes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closes {
    /// The pane alone: something else holds its tab open.
    Pane,
    /// Its tab, by its id, and the terminals in it, the pane's own among
    /// them, and the floating ones: nothing else holds the tab open.
    Tab { id: u32, panes: Vec<u32> },
    /// Its session: nothing else holds the tab open, and the session has no
    /// other tab.
    Session,
}

/// What closing the terminal `pane` of `session` closes, as zellij lists the
/// session's panes now, or why it closes nothing.
pub fn closes_now(session: &SessionName, pane: u32) -> Result<Closes, String> {
    laid(session.as_str()).and_then(|laid| closing(&laid, pane))
}

/// What closing the terminal `pane` closes among `laid`, the panes of its
/// session, or why it closes nothing: it is gone.
pub(crate) fn closing(laid: &[Laid], pane: u32) -> Result<Closes, String> {
    match together(laid, &BTreeSet::from([pane]))? {
        Closing::Session => Ok(Closes::Session),
        Closing::Parts { mut tabs, .. } => {
            Ok(tabs.pop().map_or(Closes::Pane, |(id, panes)| Closes::Tab { id, panes }))
        },
    }
}

/// What closing some terminals of a session closes, together, as zellij
/// closes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closing {
    /// Each tab that nothing left in it holds open, by its id, with its
    /// terminals, the floating ones too; and each terminal whose tab
    /// something else holds open, alone.
    Parts { tabs: Vec<(u32, Vec<u32>)>, panes: Vec<u32> },
    /// Every tab of the session, and the session with them.
    Session,
}

/// What closing the terminals `panes` of `session` closes, together, as
/// zellij lists the session's panes now, or why it closes nothing.
pub fn closes_together(session: &SessionName, panes: &BTreeSet<u32>) -> Result<Closing, String> {
    laid(session.as_str()).and_then(|laid| together(&laid, panes))
}

/// What closing the terminals `panes` closes among `laid`, the panes of their
/// session, together: a tab goes once nothing left in it holds it open, and
/// the session once all its tabs do. Or why it closes nothing: one is gone.
fn together(laid: &[Laid], panes: &BTreeSet<u32>) -> Result<Closing, String> {
    let given = |laid: &Laid| !laid.is_plugin && panes.contains(&laid.id);
    let listed = |pane: u32| laid.iter().any(|laid| given(laid) && laid.id == pane);
    if let Some(gone) = panes.iter().find(|pane| !listed(**pane)) {
        return Err(format!("pane {gone} is gone"));
    }
    let tabs = tabs(laid);
    let in_tab = |tab: u32| laid.iter().filter(move |laid| laid.tab_id == tab);
    let touched = tabs.iter().copied().filter(|tab| in_tab(*tab).any(given));
    let held = |tab: &u32| in_tab(*tab).any(|laid| !given(laid) && laid.holds_tab());
    let (held, closed): (Vec<u32>, Vec<u32>) = touched.partition(held);
    if closed.len() == tabs.len() {
        return Ok(Closing::Session);
    }
    let tabs = closed.into_iter().map(|tab| (tab, terminals(laid, tab))).collect();
    let alone = |tab| in_tab(tab).filter(|laid| given(laid)).map(|laid| laid.id);
    let panes = held.into_iter().flat_map(alone).collect();
    Ok(Closing::Parts { tabs, panes })
}

/// The terminals in the tab `tab` among `laid`, the panes of its session,
/// the floating ones too, as zellij lists them.
pub(crate) fn terminals(laid: &[Laid], tab: u32) -> Vec<u32> {
    let terminals = laid.iter().filter(|laid| laid.tab_id == tab && !laid.is_plugin);
    terminals.map(|laid| laid.id).collect()
}

/// What zellij closes of a session, short of the session, as what closing
/// a pane closes said it would: a terminal alone, by its id, or a tab, by
/// its id, with the terminals in it, the floating ones too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Close {
    Pane(u32),
    Tab { id: u32, panes: Vec<u32> },
}

impl Close {
    /// The tab `id` of a session, with the terminals `laid`, its panes,
    /// lists in it.
    #[must_use]
    pub fn tab(laid: &[Laid], id: u32) -> Self {
        Close::Tab { id, panes: terminals(laid, id) }
    }

    /// The question that has zellij close it of `session` on the machine
    /// `on` names, or on the daemon's own, where it names none.
    #[must_use]
    pub fn asked(
        &self,
        on: Option<On>,
        session: &SessionName,
        started: SystemTime,
    ) -> CloseRequest {
        let closes = match self {
            Close::Pane(pane) => close_request::Closes::Pane(*pane),
            Close::Tab { id, panes } => {
                close_request::Closes::Tab(closes_response::Tab { id: *id, panes: panes.clone() })
            },
        };
        let started_at = Some(started.into());
        CloseRequest { on, session: session.to_string(), closes: Some(closes), started_at }
    }
}

/// Closes `close` of `session`, the one that started at `started`, as
/// `closed_as_listed` does, or says why it closed nothing: one that took the
/// name since has panes of the same ids, as zellij numbers them from 0 for each
/// server, and they stay.
pub fn closed(session: &SessionName, started: SystemTime, close: &Close) -> Result<(), String> {
    same_session(session, started)?;
    closed_as_listed(session, close)
}

/// Whether `session` is the one that started at `started`, as far as the
/// sockets of those that run say; or why not, or why luneta cannot tell. One
/// that no longer runs took no name, and what asks of it hears that from
/// zellij.
pub(crate) fn same_session(session: &SessionName, started: SystemTime) -> Result<(), String> {
    let running = running_in(&crate::zellij::folder())?;
    if running.iter().any(|(name, at)| name == session.as_str() && *at != started) {
        return Err(format!("\"{session}\" is a session made since, which stays"));
    }
    Ok(())
}

/// Closes `close` of `session`, while zellij lists the session's panes as
/// it did for the plan, and says why not when zellij did not, or why it
/// closed nothing: a pane that would take its tab along now, or a tab that
/// holds other terminals than it did, or that would take the session along
/// now, stays. zellij closes nothing on a condition of its own: what changes
/// between the list and the close goes with it.
pub(crate) fn closed_as_listed(session: &SessionName, close: &Close) -> Result<(), String> {
    still(&laid(session.as_str())?, close)?;
    let mut command = Command::new("zellij");
    command.args(["--session", session.as_str(), "action"]);
    match close {
        Close::Pane(pane) => command.args(["close-pane", "--pane-id", &format!("terminal_{pane}")]),
        Close::Tab { id, .. } => command.args(["close-tab-by-id", &id.to_string()]),
    };
    done(command)
}

/// Whether `laid`, the panes of a session, lists `close` as it was planned,
/// or why it does not.
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
            // zellij ends a session with its last tab.
            if tabs(laid).iter().all(|tab| tab == id) {
                return Err(format!("tab {id} would take the session along now, so it stays"));
            }
            Ok(())
        },
    }
}

/// What `delete-session --force` says as it kills a session that runs:
/// `assert_dead_session` in zellij-utils 0.45.1's `sessions.rs` says it, on
/// stdout, of one it finds a server of.
const KILLING: &str = "will be force killed and deleted";

/// How `delete-session` ends where zellij keeps nothing of the session:
/// `delete_session` in the same file ends so where it finds no folder of the
/// session to take away.
const UNKEPT: i32 = 2;

/// How long the server of a session zellij killed has to take its socket
/// away: it does within milliseconds.
const GOING: Duration = Duration::from_secs(1);

/// How often luneta looks meanwhile.
const GOING_LOOK: Duration = Duration::from_millis(10);

/// How long a server goes on once its socket is gone, and its writer with
/// it, which writes what zellij keeps of the session anew where it finds it
/// gone: a few milliseconds, measured, and far less than this.
const SETTLING: Duration = Duration::from_millis(100);

/// Kills `session`, the one that started at `started`, and deletes it, so
/// that zellij keeps nothing to bring it back with, and says why not when it
/// did not. A client in it ends with it: luneta's own takes it to another
/// session. A session that took its name since is another, which zellij
/// would kill all the same: it stays. zellij first keeps anything of a
/// session a second after it starts: it kills a younger one all the same,
/// and fails to delete what it does not keep.
///
/// zellij takes away what it keeps before the server has ended, and the
/// server's writer can write it anew, so it is gone only once the server
/// is, as `settled` waits for: a luneta, or a `luneta close` in a pane of
/// the session, that ends with the session may not be there by then.
pub fn killed(session: &SessionName, started: SystemTime) -> Result<(), String> {
    same_session(session, started)?;
    let folder = crate::zellij::folder();
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["delete-session", "--force", session.as_str()]);
    let ran = program::run(&mut command, ANSWER).map_err(|error| unrun(&error))?;
    let unkept = matches!(&ran, program::Ran::Ended { status, stdout, .. }
        if status.code() == Some(UNKEPT) && String::from_utf8_lossy(stdout).contains(KILLING));
    if !unkept {
        ran.answer().map_err(|why| format!("zellij: {why}"))?;
    }
    settled(session, started, &folder)
}

/// Waits for the server of `session`, the one that started at `started`, to
/// end, once zellij has killed it, and has zellij let go again of what its
/// writer wrote of it meanwhile. What zellij keeps of a session that took
/// the name since is that one's, and stays. How zellij ends says too little:
/// it drops the word to the server that it fails to send, and ends well
/// where it fails to take away what it keeps, but for finding none. What is
/// in `folder`, zellij's sockets, and in its folder of what it keeps, says.
fn settled(session: &SessionName, started: SystemTime, folder: &Path) -> Result<(), String> {
    let deadline = Instant::now() + GOING;
    while running_in(folder)?.iter().any(|(name, at)| name == session.as_str() && *at == started) {
        if Instant::now() >= deadline {
            return Err(format!("zellij did not kill \"{session}\""));
        }
        thread::sleep(GOING_LOOK);
    }
    thread::sleep(SETTLING);
    let kept = crate::metadata::folder().is_some_and(|info| info.join(session.as_str()).exists());
    if kept && running_in(folder)?.iter().all(|(name, _)| name != session.as_str()) {
        return deleted(session);
    }
    Ok(())
}

/// The sessions that run, each by its name and when it started, as their
/// sockets in `folder` say now; or why luneta cannot tell them, as a
/// session's socket could be in what it cannot read.
pub fn running_in(folder: &Path) -> Result<Vec<(String, SystemTime)>, String> {
    crate::zellij::running(folder).ok_or_else(|| {
        let folder = folder.display();
        format!("luneta cannot read all of {folder}, where zellij keeps its sessions' sockets")
    })
}

/// Has zellij let go of `session`, one that ended, which then cannot be
/// brought back, or says why it did not. zellij keeps one that runs.
pub fn deleted(session: &SessionName) -> Result<(), String> {
    let mut command = Command::new("zellij");
    command.args(["delete-session", session.as_str()]);
    done(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid(id: u32, tab_id: u32) -> Laid {
        Laid {
            id,
            is_plugin: false,
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: false,
            is_fullscreen: false,
            title: String::new(),
            tab_id,
            tab_position: tab_id,
            tab_name: String::new(),
            pane_command: None,
        }
    }

    /// zellij closes a tab with its last pane that holds it open, its
    /// floating panes and all, and a session with its last tab. A bar holds
    /// none open, a plugin's pane you can go to does, and so does a terminal
    /// another took the place of, which comes back.
    #[test]
    fn the_last_pane_that_holds_its_tab_open_closes_the_tab_or_the_session() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let bar = Laid { is_plugin: true, is_selectable: false, ..laid(1, 0) };
        let plugin = Laid { is_plugin: true, ..laid(4, 0) };
        let suspended = Laid { is_suppressed: true, ..laid(7, 0) };
        let other_tab = laid(9, 1);
        let tab = |panes: Vec<u32>| Ok(Closes::Tab { id: 0, panes });
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

    /// Panes closed together close a tab once nothing left in it holds it
    /// open, as the last of them would close it, and the session once all its
    /// tabs go; a pane whose tab something else holds open closes alone.
    #[test]
    fn panes_closed_together_close_the_tab_none_left_holds_open() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let tab = vec![laid(4, 0), laid(6, 0), floating, laid(7, 1), laid(8, 1)];
        let parts = |tabs: Vec<(u32, Vec<u32>)>, panes: Vec<u32>| Ok(Closing::Parts { tabs, panes });
        for (given, closes) in [
            (vec![4, 6], parts(vec![(0, vec![4, 6, 5])], vec![])),
            (vec![4, 7], parts(vec![], vec![4, 7])),
            (vec![4, 6, 7], parts(vec![(0, vec![4, 6, 5])], vec![7])),
            (vec![4, 6, 7, 8], Ok(Closing::Session)),
            (vec![4, 9], Err("pane 9 is gone".to_owned())),
        ] {
            let given: BTreeSet<u32> = given.iter().copied().collect();
            assert_eq!(together(&tab, &given), closes, "panes {given:?}");
        }
    }

    /// A close goes ahead only while zellij lists what it closes as it was
    /// planned: a pane whose tab something else holds open, and a tab with
    /// the terminals it had, in any order, beside another tab. A pane opened
    /// in the tab since, the pane that held it open gone, or every other tab
    /// gone, as zellij ends a session with its last, has it close nothing.
    #[test]
    fn a_close_goes_ahead_only_while_zellij_lists_what_was_planned() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let bar = Laid { is_plugin: true, is_selectable: false, ..laid(1, 0) };
        let planned = vec![laid(4, 0), laid(6, 0), floating.clone(), bar, laid(9, 1)];
        let opened = [planned.clone(), vec![laid(7, 0)]].concat();
        let last = planned[..4].to_vec();
        let left = vec![laid(4, 0), floating, laid(9, 1)];
        let tab = || Close::Tab { id: 0, panes: vec![6, 5, 4] };
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
