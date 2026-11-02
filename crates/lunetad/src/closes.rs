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

use crate::cli::{done, laid, quiet, tabs, unrun, Laid, ANSWER};
use crate::program;

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
pub fn closes_now(session: &str, pane: u32) -> Result<Closes, String> {
    laid(session).and_then(|laid| closing(&laid, pane))
}

/// What closing the terminal `pane` closes among `laid`, the panes of its
/// session, or why it closes nothing: it is gone.
fn closing(laid: &[Laid], pane: u32) -> Result<Closes, String> {
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
pub fn closes_together(session: &str, panes: &BTreeSet<u32>) -> Result<Closing, String> {
    laid(session).and_then(|laid| together(&laid, panes))
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
    let terminals = |tab| in_tab(tab).filter(|laid| !laid.is_plugin).map(|laid| laid.id).collect();
    let tabs = closed.into_iter().map(|tab| (tab, terminals(tab))).collect();
    let alone = |tab| in_tab(tab).filter(|laid| given(laid)).map(|laid| laid.id);
    let panes = held.into_iter().flat_map(alone).collect();
    Ok(Closing::Parts { tabs, panes })
}

/// What zellij closes of a session, short of the session: a terminal, by
/// its id, or a tab, by its id, its panes and all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    Pane(u32),
    Tab(u32),
}

/// Closes `close` of `session`, and says why not when zellij did not.
pub fn closed(session: &str, close: Close) -> Result<(), String> {
    let mut command = Command::new("zellij");
    command.args(["--session", session, "action"]);
    match close {
        Close::Pane(pane) => command.args(["close-pane", "--pane-id", &format!("terminal_{pane}")]),
        Close::Tab(tab) => command.args(["close-tab-by-id", &tab.to_string()]),
    };
    done(command)
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
pub fn killed(session: &str, started: SystemTime) -> Result<(), String> {
    let folder = crate::zellij::folder();
    if running_in(&folder)?.iter().any(|(name, at)| name == session && *at != started) {
        return Err(format!("\"{session}\" is a session made since, which stays"));
    }
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["delete-session", "--force", session]);
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
fn settled(session: &str, started: SystemTime, folder: &Path) -> Result<(), String> {
    let deadline = Instant::now() + GOING;
    while running_in(folder)?.iter().any(|(name, at)| name == session && *at == started) {
        if Instant::now() >= deadline {
            return Err(format!("zellij did not kill \"{session}\""));
        }
        thread::sleep(GOING_LOOK);
    }
    thread::sleep(SETTLING);
    let kept = crate::metadata::folder().is_some_and(|info| info.join(session).exists());
    if kept && running_in(folder)?.iter().all(|(name, _)| name != session) {
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
pub fn deleted(session: &str) -> Result<(), String> {
    let mut command = Command::new("zellij");
    command.args(["delete-session", session]);
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
            tab_id,
            tab_position: tab_id,
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
}
