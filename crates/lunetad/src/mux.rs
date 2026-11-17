use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::process;
use crate::watch::Troubles;
use crate::Directory;

mod laid;
pub mod own;
mod zellij;

pub use laid::{tabs, terminal_of, Laid, PaneId, TabId};
pub use own::{OwnClientId, OwnId, OwnKey, OwnMade, OwnPane, TerminalKey};
pub use zellij::cli::{Asks, Client, Failure, Listed, Runs};
pub use zellij::metadata::{CacheFolder, Metadata, Unknown};
pub use zellij::{Exited, Id, Scan, SessionName, Sessions, Socket, SocketFolder, NAME_BYTES_MAX};

#[derive(Clone, Copy, Debug)]
pub enum Mux<'a> {
    Zellij(&'a SessionName),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    Zellij(ZellijPane),
    Terminal(TerminalKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijPane {
    pub session: String,
    pub pane: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneVars {
    named: Option<String>,
    pane: Option<u32>,
}

impl PaneVars {
    #[must_use]
    pub fn spelled(session: Option<String>, pane: Option<&str>) -> PaneVars {
        let named = session.filter(|session| !session.is_empty());
        PaneVars { named, pane: pane.and_then(|pane| pane.parse().ok()) }
    }

    #[must_use]
    pub fn named(&self) -> Option<&str> {
        self.named.as_deref()
    }

    #[must_use]
    pub fn pane(&self) -> Option<u32> {
        self.pane
    }

    #[must_use]
    pub fn pane_named(self) -> Option<ZellijPane> {
        Some(ZellijPane { session: self.named?, pane: self.pane? })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClientWords {
    pub program: &'static str,
    pub args: Vec<String>,
    pub unset: &'static [&'static str],
}

impl Mux<'_> {
    pub fn laid(self) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::laid(session),
        }
    }

    pub fn panes_listed(self) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_listed(session),
        }
    }

    pub fn clients(self) -> Result<Listed, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::clients(session),
        }
    }

    pub fn has_clients(self) -> Result<bool, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::has_clients(session),
        }
    }

    pub fn client_rows(self) -> Result<Vec<Client>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::client_rows(session),
        }
    }

    pub fn pane_opened(self, beside: u32, cwd: &Directory, runs: Runs) -> Result<u32, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_opened(session, beside, cwd, runs),
        }
    }

    pub fn pane_opened_once_heard<const N: usize>(
        self,
        tab: TabId,
        name: &str,
        runs: &[&str; N],
        found: impl Fn() -> Option<u32>,
    ) -> Result<u32, Failure> {
        match self {
            Mux::Zellij(session) => {
                zellij::cli::pane_opened_once_heard(session, tab, name, runs, found)
            },
        }
    }

    pub fn tab_opened(
        self,
        cwd: &Directory,
        name: Option<&str>,
        runs: Runs,
    ) -> Result<TabId, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_opened(session, cwd, name, runs),
        }
    }

    pub fn tab_opened_once_heard<const N: usize>(
        self,
        name: Option<&str>,
        runs: &[&str; N],
    ) -> Result<TabId, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_opened_once_heard(session, name, runs),
        }
    }

    pub fn pane_closed(self, pane: PaneId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_closed(session, pane, asks),
        }
    }

    pub fn tab_closed(self, tab: TabId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_closed(session, tab, asks),
        }
    }

    pub fn fullscreen_toggled(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::fullscreen_toggled(session, pane),
        }
    }

    pub fn panes_stacked<const N: usize>(self, panes: [PaneId; N]) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_stacked(session, panes),
        }
    }

    pub fn renamed(self, to: &SessionName) -> Result<(), String> {
        match self {
            Mux::Zellij(session) => zellij::renamed(session, to),
        }
    }

    pub fn screen_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::screen_dumped(session, pane),
        }
    }

    pub fn text_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::text_dumped(session, pane),
        }
    }

    pub fn session_killed(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_killed(session),
        }
    }

    pub fn session_deleted(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_deleted(session),
        }
    }

    pub fn pane_focused(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_focused(session, pane),
        }
    }

    pub fn went(self, to: &SessionName, pane: Option<u32>) -> Result<(), Failure> {
        match self {
            Mux::Zellij(at) => zellij::cli::went(at, to, pane),
        }
    }

    pub fn started(self, to: &SessionName, cwd: Option<&Directory>) -> Result<(), Failure> {
        match self {
            Mux::Zellij(at) => zellij::cli::started(at, to, cwd),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Zellij;

impl Zellij {
    pub fn here() -> Result<PaneVars, String> {
        zellij::vars::here()
    }

    #[must_use]
    pub fn may_be_in_a_pane() -> bool {
        zellij::vars::may_be_in_a_pane()
    }

    pub fn exited() -> Result<Vec<Exited>, String> {
        zellij::exited()
    }

    #[must_use]
    pub fn client_making(session: &SessionName) -> ClientWords {
        zellij::cli::words(zellij::cli::client_making(session))
    }

    pub fn client_rejoining(session: &SessionName) -> Result<ClientWords, String> {
        zellij::cli::client_rejoining(session).map(zellij::cli::words)
    }

    #[must_use]
    pub fn attaches_to(session: &SessionName) -> bool {
        zellij::cli::attaches_to(session)
    }

    #[must_use]
    pub fn nudge(kitty_keys: bool) -> Option<&'static [u8]> {
        zellij::client::nudge(kitty_keys)
    }

    #[must_use]
    pub fn nudged() -> Duration {
        zellij::client::NUDGED
    }

    #[must_use]
    pub fn dropped(said: &str) -> bool {
        zellij::client::dropped(said)
    }

    #[must_use]
    pub fn socket_folder() -> SocketFolder {
        zellij::folder()
    }

    #[must_use]
    pub fn of_sessions(path: &Path) -> bool {
        zellij::of_sessions(path)
    }

    #[must_use]
    pub fn newborn(started: impl Iterator<Item = SystemTime>, now: SystemTime) -> Option<Duration> {
        zellij::newborn(started, now)
    }

    #[must_use]
    pub fn cache_folder() -> Option<CacheFolder> {
        zellij::metadata::folder()
    }

    #[must_use]
    pub fn is_metadata(path: &Path) -> bool {
        zellij::metadata::is_metadata(path)
    }

    pub fn kept() -> Result<Vec<SessionName>, Unknown> {
        zellij::metadata::kept()
    }

    pub fn cached(session: &SessionName) -> Result<bool, Unknown> {
        zellij::metadata::cached(session)
    }
}

#[must_use]
pub fn pane_of(pid: u32) -> Option<Named> {
    pane_in(&process::environ(pid)?)
}

fn pane_in(environ: &[OsString]) -> Option<Named> {
    let terminal = || own::terminal_in(environ).map(Named::Terminal);
    zellij::vars::pane_in(environ).or_else(terminal)
}

fn value_in<'a>(environ: &'a [OsString], name: &str) -> Option<&'a [u8]> {
    environ.iter().find_map(|entry| {
        entry.as_bytes().strip_prefix(name.as_bytes())?.strip_prefix(b"=")
    })
}

pub fn in_no_pane(command: &mut Command) {
    zellij::vars::in_no_pane(command);
    own::in_no_pane(command);
}

impl SocketFolder {
    pub fn scan(&self, troubles: &mut Troubles) -> Scan {
        zellij::scan(self, troubles)
    }

    #[must_use]
    pub fn running(&self) -> Option<Vec<(SessionName, SystemTime)>> {
        zellij::running(self)
    }

    #[must_use]
    pub fn sockets(&self) -> Option<Vec<(SessionName, Id)>> {
        zellij::sockets(self)
    }
}

impl CacheFolder {
    pub fn metadata(&self, name: &SessionName) -> io::Result<Option<Metadata>> {
        zellij::metadata::read(self, name)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn environ(entries: &[&str]) -> Vec<OsString> {
        entries.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_pane_of_zellij_s_in_a_terminal_is_zellij_s() {
        let terminal = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(pane_in(&environ(&terminal)), Some(Named::Terminal(key)));
        let zellij = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&[&terminal[..], &zellij].concat())), Some(named));
        assert_eq!(pane_in(&environ(&["HOME=/h"])), None);
        let old = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        assert_eq!(pane_in(&environ(&old)), None, "an own session's pane is no pane now");
    }

    #[test]
    fn a_variable_set_twice_reads_as_its_first() {
        let twice = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2", "ZELLIJ_PANE_ID=5"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&twice)), Some(named));
        let twice = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let twice = [&twice[..], &["LUNETA_MUX_TERMINAL=8"]].concat();
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(pane_in(&environ(&twice)), Some(Named::Terminal(key)));
    }
}
