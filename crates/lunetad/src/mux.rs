use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::closes::Whose;
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
    Own(&'a own::server::Own),
}

#[must_use]
pub fn two_named(name: &str) -> String {
    format!("two sessions are named {name}: one of zellij's and an own one")
}

#[derive(Debug)]
pub enum Called<'a> {
    Zellij(&'a SessionName),
    Own(Box<own::server::Own>),
}

impl<'a> Called<'a> {
    pub fn named(name: &'a SessionName) -> Result<Called<'a>, Failure> {
        let Some(own) = own::server::named(name)? else {
            return Ok(Called::Zellij(name));
        };
        let zellij = crate::closes::running_in(&Zellij::socket_folder()).map_err(Failure::Unread)?;
        if zellij.iter().any(|(running, _)| running == name) {
            return Err(Failure::Unread(two_named(name.as_str())));
        }
        Ok(Called::Own(Box::new(own)))
    }

    pub fn of(name: &'a SessionName, whose: Whose) -> Result<Called<'a>, Failure> {
        match whose {
            Whose::Zellij(_) => Ok(Called::Zellij(name)),
            Whose::Own(key) => Ok(Called::Own(Box::new(own::server::keyed(key)?))),
        }
    }

    #[must_use]
    pub fn mux(&self) -> Mux<'_> {
        match self {
            Called::Zellij(name) => Mux::Zellij(name),
            Called::Own(own) => Mux::Own(own),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    Zellij(ZellijPane),
    Own(OwnPane),
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
            Mux::Own(session) => session.laid(),
        }
    }

    pub fn panes_listed(self) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_listed(session),
            Mux::Own(session) => session.laid(),
        }
    }

    pub fn clients(self) -> Result<Listed, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::clients(session),
            Mux::Own(session) => session.clients(),
        }
    }

    pub fn has_clients(self) -> Result<bool, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::has_clients(session),
            Mux::Own(session) => Ok(!session.clients()?.is_empty()),
        }
    }

    pub fn client_rows(self) -> Result<Vec<Client>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::client_rows(session),
            Mux::Own(session) => Ok(session.clients()?.to_vec()),
        }
    }

    pub fn pane_opened(self, beside: u32, cwd: &Directory, runs: Runs) -> Result<u32, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_opened(session, beside, cwd, runs),
            Mux::Own(_) => Err(Failure::Unsupported(BESIDE)),
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
            Mux::Own(_) => Err(Failure::Unsupported(BESIDE)),
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
            Mux::Own(session) => session.tab_opened(cwd, name, runs),
        }
    }

    pub fn tab_opened_once_heard<const N: usize>(
        self,
        name: Option<&str>,
        runs: &[&str; N],
    ) -> Result<TabId, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_opened_once_heard(session, name, runs),
            Mux::Own(session) => session.tab_opened_once_heard(name, runs),
        }
    }

    pub fn pane_closed(self, pane: PaneId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_closed(session, pane, asks),
            Mux::Own(_) => Err(Failure::Unsupported("close a pane apart from its tab")),
        }
    }

    pub fn tab_closed(self, tab: TabId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_closed(session, tab, asks),
            Mux::Own(session) => session.tab_closed(tab),
        }
    }

    pub fn fullscreen_toggled(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::fullscreen_toggled(session, pane),
            Mux::Own(_) => Err(Failure::Unsupported("show a pane over the others of its tab")),
        }
    }

    pub fn panes_stacked<const N: usize>(self, panes: [PaneId; N]) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_stacked(session, panes),
            Mux::Own(_) => Err(Failure::Unsupported("stack panes")),
        }
    }

    pub fn renamed(self, to: &SessionName) -> Result<(), String> {
        match self {
            Mux::Zellij(session) => {
                if own::server::named(to)?.is_some() {
                    return Err(format!("{to} runs already, as an own session"));
                }
                zellij::renamed(session, to)
            },
            Mux::Own(session) => {
                crate::names::unused_by_zellij(to)?;
                Ok(session.renamed(to)?)
            },
        }
    }

    pub fn screen_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::screen_dumped(session, pane),
            Mux::Own(session) => session.dumped(pane, own::api::Dumped::Screen),
        }
    }

    pub fn text_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::text_dumped(session, pane),
            Mux::Own(session) => session.dumped(pane, own::api::Dumped::Text),
        }
    }

    pub fn session_killed(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_killed(session),
            Mux::Own(session) => session.killed(),
        }
    }

    pub fn session_deleted(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_deleted(session),
            Mux::Own(_) => Err(Failure::Unsupported("keep a session that ended")),
        }
    }

    pub fn pane_focused(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_focused(session, pane),
            Mux::Own(session) => {
                let pane = pane.terminal().ok_or(Failure::Unsupported("hold a plugin's pane"))?;
                let pane = own::server::pane_of(session.key(), pane)?;
                session.went(own::server::Mover::LastTyped, own::server::Place::Pane(pane))
            },
        }
    }

    pub fn went(self, to: &SessionName, pane: Option<u32>) -> Result<(), Failure> {
        match self {
            Mux::Zellij(_) if own::server::named(to)?.is_some() => {
                Err(Failure::Unsupported(ACROSS))
            },
            Mux::Zellij(at) => zellij::cli::went(at, to, pane),
            Mux::Own(at) => {
                let Some(to) = at.named(to)? else {
                    return Err(unowned(to)?);
                };
                let place = match pane {
                    None => own::server::Place::Session(to),
                    Some(pane) => own::server::Place::Pane(own::server::pane_of(to, pane)?),
                };
                at.went(own::server::Mover::LastTyped, place)
            },
        }
    }

    pub fn started(self, to: &SessionName, cwd: Option<&Directory>) -> Result<(), Failure> {
        match self {
            Mux::Zellij(_) if own::server::named(to)?.is_some() => {
                Err(Failure::Unsupported(ACROSS))
            },
            Mux::Zellij(at) => zellij::cli::started(at, to, cwd),
            Mux::Own(_) => Err(Failure::Unsupported("make a session from an own one")),
        }
    }
}

const BESIDE: &str = "open a pane beside another";

const ACROSS: &str = "take a client between one of zellij's sessions and an own one";

fn unowned(to: &SessionName) -> Result<Failure, Failure> {
    let running = crate::closes::running_in(&Zellij::socket_folder()).map_err(Failure::Unread)?;
    let unknown = |why| format!("luneta cannot tell whether zellij keeps {to}: {why}");
    let kept = Zellij::kept().map_err(|why| Failure::Unread(unknown(why)))?;
    if running.iter().any(|(name, _)| name == to) || kept.contains(to) {
        return Ok(Failure::Unsupported(ACROSS));
    }
    Ok(Failure::Own(format!("no session is named {to}, of its own or of zellij's")))
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
    let own = || own::pane_in(environ).map(Named::Own);
    let terminal = || own::terminal_in(environ).map(Named::Terminal);
    zellij::vars::pane_in(environ).or_else(own).or_else(terminal)
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

    #[test]
    fn a_pane_of_zellij_s_in_an_own_pane_is_zellij_s() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let own = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        let made = OwnMade::from_nanos(60);
        let session = OwnKey { id: OwnId::new(3).unwrap(), made };
        assert_eq!(pane_in(&environ(&own)), OwnPane::new(session, 12).map(Named::Own));
        let zellij = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&[&own[..], &zellij].concat())), Some(named));
        assert_eq!(pane_in(&environ(&["HOME=/h"])), None);
        let terminal = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made };
        assert_eq!(pane_in(&environ(&terminal)), Some(Named::Terminal(key)));
        let both = pane_in(&environ(&[&own[..], &terminal].concat()));
        assert_eq!(both, OwnPane::new(session, 12).map(Named::Own));
    }

    #[test]
    fn a_variable_set_twice_reads_as_its_first() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let twice = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2", "ZELLIJ_PANE_ID=5"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&twice)), Some(named));
        let twice = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        let twice = [&twice[..], &["LUNETA_MUX_SESSION=4"]].concat();
        let made = OwnMade::from_nanos(60);
        let session = OwnKey { id: OwnId::new(3).unwrap(), made };
        assert_eq!(pane_in(&environ(&twice)), OwnPane::new(session, 12).map(Named::Own));
    }
}
