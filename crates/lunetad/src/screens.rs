//! What a terminal of a session of the machine zellij runs on shows, as
//! zellij dumps its screen, for luneta's picker to show. luneta asks it of
//! its own machine, and a daemon of its machine for a luneta on another,
//! whose picker shows a session or an agent of this one.

use std::process::Command;

use crate::cli::{answered, laid, quiet, Laid};
use crate::zellij::SessionName;

/// The screen of the terminal `pane` of `session`, or of the one a client
/// coming there would be on, where it asks of none, as `focused` says: the
/// terminal, and its lines, in the colours it shows them in, as zellij
/// dumps them; or why zellij did not.
pub fn dumped(session: &SessionName, pane: Option<u32>) -> Result<(u32, String), String> {
    let pane = pane.map_or_else(|| focused(session), Ok)?;
    let mut command = Command::new("zellij");
    quiet(&mut command);
    let terminal = format!("terminal_{pane}");
    let session = session.as_str();
    command.args(["--session", session, "action", "dump-screen", "--pane-id", &terminal, "--ansi"]);
    Ok((pane, answered(&mut command)?))
}

/// The terminal of `session` a client coming there would be on, as zellij
/// lists its panes now: in its first tab, the one focused there, or else the
/// first, a tiled one before one floating. zellij says which tab a session
/// is on only while a client is on it.
fn focused(session: &SessionName) -> Result<u32, String> {
    let laid = laid(session.as_str())?;
    shown(&laid).ok_or_else(|| format!("{session} has no terminal to show"))
}

/// The terminal among `laid`, the panes of a session, that a client coming
/// there would be on, as `focused` says.
fn shown(laid: &[Laid]) -> Option<u32> {
    let terminal = |laid: &&Laid| !laid.is_plugin && laid.is_selectable && !laid.is_suppressed;
    let rank = |laid: &&Laid| (laid.tab_position, laid.is_floating, !laid.is_focused);
    laid.iter().filter(terminal).min_by_key(rank).map(|laid| laid.id)
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
            tab_id,
            tab_position: tab_id,
            tab_name: String::new(),
            pane_command: None,
        }
    }

    /// A session with no client shows the terminal focused in its first tab,
    /// a tiled one over one floating, or else the first there: no other tab
    /// is the one zellij says it is on.
    #[test]
    fn a_session_with_no_client_shows_the_pane_focused_in_its_first_tab() {
        let focused = |laid: Laid| Laid { is_focused: true, ..laid };
        let floating = |laid: Laid| Laid { is_floating: true, ..laid };
        let plugin = |laid: Laid| Laid { is_plugin: true, ..laid };
        let tabs = [laid(1, 1), focused(laid(2, 1)), focused(laid(3, 0)), laid(4, 0)];
        assert_eq!(shown(&tabs), Some(3));
        let first = [focused(floating(laid(5, 0))), laid(6, 0), laid(7, 0)];
        assert_eq!(shown(&first), Some(6));
        assert_eq!(shown(&[plugin(laid(8, 0))]), None);
    }
}
