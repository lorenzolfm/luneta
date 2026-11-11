//! The multiplexers that hold the sessions of the machine luneta and its
//! daemon run on, in luneta's own words: what they have one do to a session,
//! and what they read of it. zellij is the only one yet. What luneta asks of
//! a session goes through `Mux`; what zellij alone has, its folders, what it
//! keeps of sessions that ended and the words of its client, through
//! `Zellij`; and which pane a process runs in, of either multiplexer, through
//! `pane_of` and `in_no_pane`. `zellij` is private.
//!
//! Not all luneta knows of zellij's ways is behind it yet. `Laid` still has
//! zellij's plugins, bars and suppressed panes, `Asks` and `Failure` are
//! zellij's, `closes` waits on zellij's sockets and cache as a session it
//! killed settles, and the app keeps the rest, in its `zellij` and `own`.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::process;
use crate::watch::Troubles;
use crate::Directory;

mod laid;
mod own;
mod zellij;

pub use laid::{tabs, terminal_of, Laid, PaneId, TabId};
pub use zellij::cli::{Asks, Client, Failure, Listed, Runs};
pub use zellij::metadata::{CacheFolder, Metadata, Unknown};
pub use zellij::{Exited, Id, Scan, SessionName, Sessions, Socket, SocketFolder, NAME_BYTES_MAX};

/// A multiplexer luneta has its sessions in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mux {
    Zellij,
}

/// A pane of a multiplexer's, as the variables of what runs in it name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    Zellij(ZellijPane),
    /// A pane of luneta's own multiplexer, by the ids its server gave its
    /// session and it, which it gives no other.
    Own { session: u64, pane: u64 },
}

/// A pane of zellij's, as the variables of what runs in it name it: by the
/// name its session had as the pane opened, which a rename leaves behind, and
/// the terminal's id there. The name is as they spell it, held to nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijPane {
    pub session: String,
    pub pane: u32,
}

/// What the variables a multiplexer gives what runs in its panes say of the
/// pane this process runs in: the name its session had as it opened, where
/// they name one, and its id, where they name a terminal's. A rename leaves
/// the name behind, so a pane goes by the id where the name does not match.
/// Only `spelled` makes one, so that a name is never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneVars {
    named: Option<String>,
    pane: Option<u32>,
}

impl PaneVars {
    /// What `session` and `pane`, as the variables spell them, say: a name,
    /// where it is not empty, and a terminal's id, where it is one.
    #[must_use]
    pub fn spelled(session: Option<String>, pane: Option<&str>) -> PaneVars {
        let named = session.filter(|session| !session.is_empty());
        PaneVars { named, pane: pane.and_then(|pane| pane.parse().ok()) }
    }

    /// The name the session had as the pane opened, where they name one.
    #[must_use]
    pub fn named(&self) -> Option<&str> {
        self.named.as_deref()
    }

    /// The pane's id, where they name a terminal's.
    #[must_use]
    pub fn pane(&self) -> Option<u32> {
        self.pane
    }

    /// The pane, where the variables name both its session and its id.
    #[must_use]
    pub fn pane_named(self) -> Option<ZellijPane> {
        Some(ZellijPane { session: self.named?, pane: self.pane? })
    }
}

/// The words that start a multiplexer's client: `program`, with `args`, run
/// without the variables `unset` names.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClientWords {
    pub program: &'static str,
    pub args: Vec<String>,
    pub unset: &'static [&'static str],
}

impl Mux {
    /// The panes of `session` as it lays them out now, the plugins' you can
    /// go to too.
    pub fn laid(self, session: &SessionName) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij => zellij::cli::laid(session),
        }
    }

    /// Every pane of `session` as it lays them out now, the bars too, asked
    /// once.
    pub fn panes_listed(self, session: &SessionName) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij => zellij::cli::panes_listed(session),
        }
    }

    /// The clients of `session` now, each on the pane it has the focus on.
    pub fn clients(self, session: &SessionName) -> Result<Listed, Failure> {
        match self {
            Mux::Zellij => zellij::cli::clients(session),
        }
    }

    /// Whether a client is attached to `session` now.
    pub fn has_clients(self, session: &SessionName) -> Result<bool, Failure> {
        match self {
            Mux::Zellij => zellij::cli::has_clients(session),
        }
    }

    /// As many clients of `session` as it has now, or more, none of which is
    /// one that comes to it after: the cheaper ask, for a client on its way.
    pub fn client_rows(self, session: &SessionName) -> Result<Vec<Client>, Failure> {
        match self {
            Mux::Zellij => zellij::cli::client_rows(session),
        }
    }

    /// Opens a pane beside the terminal `beside` of `session`, in its tab and
    /// in `cwd`, that runs `runs`, with no client moved, and says which
    /// terminal it opened.
    pub fn pane_opened(
        self,
        session: &SessionName,
        beside: u32,
        cwd: &Directory,
        runs: Runs,
    ) -> Result<u32, Failure> {
        match self {
            Mux::Zellij => zellij::cli::pane_opened(session, beside, cwd, runs),
        }
    }

    /// Opens a pane named `name` in the tab `tab` of `session`, with no client
    /// moved, that runs `runs` and closes as it ends, and says which: as
    /// `found` finds it, where the multiplexer does not say. It may open a
    /// second, which ends on its own.
    pub fn pane_opened_once_heard<const N: usize>(
        self,
        session: &SessionName,
        tab: TabId,
        name: &str,
        runs: &[&str; N],
        found: impl Fn() -> Option<u32>,
    ) -> Result<u32, Failure> {
        match self {
            Mux::Zellij => zellij::cli::pane_opened_once_heard(session, tab, name, runs, found),
        }
    }

    /// Opens a tab in `session`, in `cwd`, named `name` or as the multiplexer
    /// numbers it, that runs `runs`, with no client moved, and says its id.
    pub fn tab_opened(
        self,
        session: &SessionName,
        cwd: &Directory,
        name: Option<&str>,
        runs: Runs,
    ) -> Result<TabId, Failure> {
        match self {
            Mux::Zellij => zellij::cli::tab_opened(session, cwd, name, runs),
        }
    }

    /// Opens a tab in `session`, named `name` or as the multiplexer numbers
    /// it, with no client moved, that runs `runs` and closes as it ends, and
    /// says its id. It may open a second, which ends on its own.
    pub fn tab_opened_once_heard<const N: usize>(
        self,
        session: &SessionName,
        name: Option<&str>,
        runs: &[&str; N],
    ) -> Result<TabId, Failure> {
        match self {
            Mux::Zellij => zellij::cli::tab_opened_once_heard(session, name, runs),
        }
    }

    /// Closes `pane` of `session`, asked as often as `asks` says.
    pub fn pane_closed(
        self,
        session: &SessionName,
        pane: PaneId,
        asks: Asks,
    ) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::pane_closed(session, pane, asks),
        }
    }

    /// Closes the tab `tab` of `session`, asked as often as `asks` says.
    pub fn tab_closed(self, session: &SessionName, tab: TabId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::tab_closed(session, tab, asks),
        }
    }

    /// Shows `pane` of `session` over the other panes of its tab, or no
    /// longer.
    pub fn fullscreen_toggled(self, session: &SessionName, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::fullscreen_toggled(session, pane),
        }
    }

    /// Stacks `panes` of `session` on the first of them, each from whatever
    /// tab it is in. Where there is no room, it stacks nothing, and says
    /// nothing of it.
    pub fn panes_stacked<const N: usize>(
        self,
        session: &SessionName,
        panes: [PaneId; N],
    ) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::panes_stacked(session, panes),
        }
    }

    /// Renames `session` to `to`, or says why not. A session with no client
    /// takes the name as well as one with.
    pub fn renamed(self, session: &SessionName, to: &SessionName) -> Result<(), String> {
        match self {
            Mux::Zellij => zellij::renamed(session, to),
        }
    }

    /// The screen of the terminal `pane` of `session`: its lines, in the
    /// colours it shows them in.
    pub fn screen_dumped(self, session: &SessionName, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij => zellij::cli::screen_dumped(session, pane),
        }
    }

    /// The text of the terminal `pane` of `session`, its scrollback with it: a
    /// line for each line written to it, however many rows it wraps over, with
    /// no styles and no spaces at its end.
    pub fn text_dumped(self, session: &SessionName, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij => zellij::cli::text_dumped(session, pane),
        }
    }

    /// Kills `session` and lets go of what the multiplexer keeps of it.
    pub fn session_killed(self, session: &SessionName) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::session_killed(session),
        }
    }

    /// Lets go of what the multiplexer keeps of `session`, one that ended.
    pub fn session_deleted(self, session: &SessionName) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::session_deleted(session),
        }
    }

    /// Takes the client that last typed in `session` to its pane `pane`. A
    /// pane already focused is where it was to go.
    pub fn pane_focused(self, session: &SessionName, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::pane_focused(session, pane),
        }
    }

    /// Takes the client that last typed in `at` to the terminal `pane` of
    /// `to`, or to `to` alone, on the pane it last had the focus on: in `at`
    /// itself, as `pane_focused` does. A pane already focused is where it was
    /// to go.
    pub fn went(
        self,
        at: &SessionName,
        to: &SessionName,
        pane: Option<u32>,
    ) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::went(at, to, pane),
        }
    }

    /// Takes the client that last typed in `at` to a session it makes, `to`,
    /// that starts in `cwd`, or where the client started: to one that runs or
    /// ended by that name instead, which it brings back, with `cwd` unused.
    pub fn started(
        self,
        at: &SessionName,
        to: &SessionName,
        cwd: Option<&Directory>,
    ) -> Result<(), Failure> {
        match self {
            Mux::Zellij => zellij::cli::started(at, to, cwd),
        }
    }
}

/// What luneta finds and runs of zellij's alone, which no other multiplexer
/// has: the folders it keeps its sessions in, what it keeps of those that
/// ended, and the words of its client.
#[derive(Clone, Copy, Debug)]
pub struct Zellij;

impl Zellij {
    /// What the variables zellij gives what runs in its panes say of the pane
    /// this process runs in. Or why they do not read.
    pub fn here() -> Result<PaneVars, String> {
        zellij::vars::here()
    }

    /// Whether this process may run in a pane of zellij's: what it sets in its
    /// panes is set, though it may name none.
    #[must_use]
    pub fn may_be_in_a_pane() -> bool {
        zellij::vars::may_be_in_a_pane()
    }

    /// The sessions that ended which zellij keeps to bring back.
    pub fn exited() -> Result<Vec<Exited>, String> {
        zellij::exited()
    }

    /// The client, attached to `session`, which it makes where none runs, or
    /// without one, in a session of a name zellij makes up; or why
    /// it cannot attach.
    pub fn client_attaching(session: Option<&SessionName>) -> Result<ClientWords, String> {
        zellij::cli::client_attaching(session).map(zellij::cli::words)
    }

    /// The client that makes `session` in the directory it starts in, and
    /// fails where one runs by that name, or one that ended is kept by it.
    #[must_use]
    pub fn client_making(session: &SessionName) -> ClientWords {
        zellij::cli::words(zellij::cli::client_making(session))
    }

    /// The client attached to `session`, which runs, or which ended and comes
    /// back; it makes none. Or why it cannot attach.
    pub fn client_rejoining(session: &SessionName) -> Result<ClientWords, String> {
        zellij::cli::client_rejoining(session).map(zellij::cli::words)
    }

    /// Whether a client attaches to `session`.
    #[must_use]
    pub fn attaches_to(session: &SessionName) -> bool {
        zellij::cli::attaches_to(session)
    }

    /// What luneta types into the client before it asks zellij to
    /// move it, and as keys come to luneta, where zellij moves only
    /// a client that typed: none where it would not read it as a key, as
    /// `kitty_keys` says whether the client asked for keys as kitty spells
    /// them.
    #[must_use]
    pub fn nudge(kitty_keys: bool) -> Option<&'static [u8]> {
        zellij::client::nudge(kitty_keys)
    }

    /// How long an action waits after the client typed, for zellij to
    /// take the key in first.
    #[must_use]
    pub fn nudged() -> Duration {
        zellij::client::NUDGED
    }

    /// Whether `said`, what the client left on its terminal as it ended well,
    /// says that zellij dropped it, and the session runs on.
    #[must_use]
    pub fn dropped(said: &str) -> bool {
        zellij::client::dropped(said)
    }

    /// Where zellij keeps the sockets of its sessions.
    #[must_use]
    pub fn socket_folder() -> SocketFolder {
        zellij::folder()
    }

    /// Whether a change at `path`, in the socket folder, can be a session's.
    #[must_use]
    pub fn of_sessions(path: &Path) -> bool {
        zellij::of_sessions(path)
    }

    /// How long until the youngest of the sessions that `started` so is old
    /// enough to be asked anything, at `now`, where one is too young.
    #[must_use]
    pub fn newborn(started: impl Iterator<Item = SystemTime>, now: SystemTime) -> Option<Duration> {
        zellij::newborn(started, now)
    }

    /// Where zellij writes what it keeps of each session: none
    /// without a home to find it in.
    #[must_use]
    pub fn cache_folder() -> Option<CacheFolder> {
        zellij::metadata::folder()
    }

    /// Whether a change at `path`, in the cache folder, can be to a session's
    /// metadata.
    #[must_use]
    pub fn is_metadata(path: &Path) -> bool {
        zellij::metadata::is_metadata(path)
    }

    /// The sessions zellij keeps to bring back from, those that
    /// ended and those that run, by name.
    pub fn kept() -> Result<Vec<SessionName>, Unknown> {
        zellij::metadata::kept()
    }

    /// Whether zellij keeps anything of `session` in its cache.
    pub fn cached(session: &SessionName) -> Result<bool, Unknown> {
        zellij::metadata::cached(session)
    }
}

/// The pane the variables of the process `pid` name, of whichever
/// multiplexer, if they name one. zellij's go first: a zellij server that
/// started in an own pane gives its panes the variables it started with.
#[must_use]
pub fn pane_of(pid: u32) -> Option<Named> {
    pane_in(&process::environ(pid)?)
}

/// The pane `environ`, the variables of a process, name, as `pane_of` says.
fn pane_in(environ: &[OsString]) -> Option<Named> {
    zellij::vars::pane_in(environ).or_else(|| own::pane_in(environ))
}

/// Has `command` run without the variables any multiplexer gives what runs in
/// its panes: in none, as the daemon and luneta's commands see it.
pub fn in_no_pane(command: &mut Command) {
    zellij::vars::in_no_pane(command);
    own::in_no_pane(command);
}

impl SocketFolder {
    /// The sessions whose sockets are in it now. What goes wrong in reading
    /// it, `troubles` says.
    pub fn scan(&self, troubles: &mut Troubles) -> Scan {
        zellij::scan(self, troubles)
    }

    /// The sessions that run, as their sockets in it say now, each by its
    /// name and when it started. None where any of it could not be read.
    #[must_use]
    pub fn running(&self) -> Option<Vec<(SessionName, SystemTime)>> {
        zellij::running(self)
    }

    /// `running`, each by its name and which session it is, as it stays
    /// through a rename.
    #[must_use]
    pub fn sockets(&self) -> Option<Vec<(SessionName, Id)>> {
        zellij::sockets(self)
    }
}

impl CacheFolder {
    /// What zellij last wrote in it of the session `name`: none
    /// before it has.
    pub fn metadata(&self, name: &SessionName) -> io::Result<Option<Metadata>> {
        zellij::metadata::read(self, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process in a pane of zellij's whose server started in an own pane
    /// has the variables of both, and is in zellij's.
    #[test]
    fn a_pane_of_zellij_s_in_an_own_pane_is_zellij_s() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let own = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE=12"];
        assert_eq!(pane_in(&environ(&own)), Some(Named::Own { session: 3, pane: 12 }));
        let zellij = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&[&own[..], &zellij].concat())), Some(named));
        assert_eq!(pane_in(&environ(&["HOME=/h"])), None);
    }
}
