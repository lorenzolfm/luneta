use crate::mux::own::api::Dumped;
use crate::mux::own::server::Terminals;
use crate::mux::{Laid, Mux, SessionName, TerminalKey};

pub fn dumped(session: &SessionName, pane: Option<u32>) -> Result<(u32, String), String> {
    let mux = Mux::Zellij(session);
    let pane = pane.map_or_else(|| focused(mux, session), Ok)?;
    Ok((pane, mux.screen_dumped(pane)?))
}

pub fn terminal(key: TerminalKey) -> Result<String, String> {
    Ok(Terminals.dumped(key, Dumped::Screen)?)
}

fn focused(mux: Mux<'_>, session: &SessionName) -> Result<u32, String> {
    let laid = mux.laid()?;
    shown(&laid).ok_or_else(|| format!("{session} has no terminal to show"))
}

fn shown(laid: &[Laid]) -> Option<u32> {
    let on_screen = laid.iter().filter(|laid| laid.is_selectable && !laid.is_suppressed);
    let terminals = on_screen.filter_map(|laid| Some((laid, laid.pane.terminal()?)));
    let rank = |(laid, _): &(&Laid, u32)| (laid.tab_position, laid.is_floating, !laid.is_focused);
    terminals.min_by_key(rank).map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::{PaneId, TabId};

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
    fn a_session_with_no_client_shows_the_pane_focused_in_its_first_tab() {
        let focused = |laid: Laid| Laid { is_focused: true, ..laid };
        let floating = |laid: Laid| Laid { is_floating: true, ..laid };
        let tabs = [laid(1, 1), focused(laid(2, 1)), focused(laid(3, 0)), laid(4, 0)];
        assert_eq!(shown(&tabs), Some(3));
        let first = [focused(floating(laid(5, 0))), laid(6, 0), laid(7, 0)];
        assert_eq!(shown(&first), Some(6));
        assert_eq!(shown(&[Laid { pane: PaneId::Plugin(8), ..laid(8, 0) }]), None);
    }
}
