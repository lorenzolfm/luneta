//! The multiplexers that hold the sessions of the machine luneta and its
//! daemon run on, in luneta's own words: what they have one do to a session,
//! and what they read of it. They are zellij and luneta's own, whose sessions
//! share one namespace. What luneta asks of a session goes through `Mux`;
//! what zellij alone has, its folders, what it keeps of sessions that ended
//! and the words of its client, through `Zellij`; what the own multiplexer
//! alone has, its server, through `own`; and which pane a process runs in, of
//! either multiplexer, through `pane_of` and `in_no_pane`. `zellij` is
//! private.
//!
//! Not all luneta knows of zellij's ways is behind it yet. `Laid` still has
//! zellij's plugins, bars and suppressed panes, `Asks` and `Failure` are
//! zellij's, `closes` waits on zellij's sockets and cache as a session it
//! killed settles, and the app keeps the rest, in its `zellij` and `own`.

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
pub use own::OwnId;
pub use zellij::cli::{Asks, Client, Failure, Listed, Runs};
pub use zellij::metadata::{CacheFolder, Metadata, Unknown};
pub use zellij::{Exited, Id, Scan, SessionName, Sessions, Socket, SocketFolder, NAME_BYTES_MAX};

/// A session of a multiplexer's that luneta has its sessions in, by what that
/// multiplexer finds it by: zellij by its name, and luneta's own by its id,
/// on the server reached that runs it.
#[derive(Clone, Copy, Debug)]
pub enum Mux<'a> {
    Zellij(&'a SessionName),
    Own(&'a own::server::Own),
}

/// A session by its name, of whichever multiplexer runs one by it: an own
/// session, with its server reached, or else zellij's.
#[derive(Debug)]
pub enum Called<'a> {
    Zellij(&'a SessionName),
    Own(Box<own::server::Own>),
}

impl<'a> Called<'a> {
    /// The session named `name`: an own session, where one has the name, or
    /// else zellij's. Or why luneta cannot tell, as a server that does not
    /// answer could have one by the name, or zellij runs one by it too, which
    /// it makes by names it alone checks.
    pub fn named(name: &'a SessionName) -> Result<Called<'a>, Failure> {
        let Some(own) = own::server::named(name)? else {
            return Ok(Called::Zellij(name));
        };
        let zellij = crate::closes::running_in(&Zellij::socket_folder()).map_err(Failure::Unread)?;
        if zellij.iter().any(|(running, _)| running == name) {
            let two = format!("two sessions are named {name}: one of zellij's and an own one");
            return Err(Failure::Unread(two));
        }
        Ok(Called::Own(Box::new(own)))
    }

    /// The session, to ask its multiplexer of.
    #[must_use]
    pub fn mux(&self) -> Mux<'_> {
        match self {
            Called::Zellij(name) => Mux::Zellij(name),
            Called::Own(own) => Mux::Own(own),
        }
    }
}

/// A pane of a multiplexer's, as the variables of what runs in it name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    Zellij(ZellijPane),
    /// A pane of luneta's own multiplexer, by the ids its server gave its
    /// session and it, which it gives no other.
    Own { session: OwnId, pane: u32 },
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

impl Mux<'_> {
    /// The panes of the session as it lays them out now, the plugins' you can
    /// go to too.
    pub fn laid(self) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::laid(session),
            Mux::Own(session) => session.laid(),
        }
    }

    /// Every pane of the session as it lays them out now, the bars too, asked
    /// once.
    pub fn panes_listed(self) -> Result<Vec<Laid>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_listed(session),
            Mux::Own(session) => session.laid(),
        }
    }

    /// The clients of the session now, each on the pane it has the focus on.
    pub fn clients(self) -> Result<Listed, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::clients(session),
            Mux::Own(session) => session.clients(),
        }
    }

    /// Whether a client is attached to the session now.
    pub fn has_clients(self) -> Result<bool, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::has_clients(session),
            Mux::Own(session) => Ok(!session.clients()?.is_empty()),
        }
    }

    /// As many clients of the session as it has now, or more, none of which is
    /// one that comes to it after: the cheaper ask, for a client on its way.
    pub fn client_rows(self) -> Result<Vec<Client>, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::client_rows(session),
            Mux::Own(session) => Ok(session.clients()?.to_vec()),
        }
    }

    /// Opens a pane beside the terminal `beside` of the session, in its tab and
    /// in `cwd`, that runs `runs`, with no client moved, and says which
    /// terminal it opened.
    pub fn pane_opened(self, beside: u32, cwd: &Directory, runs: Runs) -> Result<u32, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_opened(session, beside, cwd, runs),
            Mux::Own(_) => Err(Failure::Unsupported(BESIDE)),
        }
    }

    /// Opens a pane named `name` in the tab `tab` of the session, with no
    /// client moved, that runs `runs` and closes as it ends, and says which: as
    /// `found` finds it, where the multiplexer does not say. It may open a
    /// second, which ends on its own.
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

    /// Opens a tab in the session, in `cwd`, named `name` or as the multiplexer
    /// numbers it, that runs `runs`, with no client moved, and says its id.
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

    /// Opens a tab in the session, named `name` or as the multiplexer numbers
    /// it, with no client moved, that runs `runs` and closes as it ends, and
    /// says its id. It may open a second, which ends on its own.
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

    /// Closes `pane` of the session, asked as often as `asks` says.
    pub fn pane_closed(self, pane: PaneId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_closed(session, pane, asks),
            Mux::Own(_) => Err(Failure::Unsupported("close a pane apart from its tab")),
        }
    }

    /// Closes the tab `tab` of the session, asked as often as `asks` says.
    pub fn tab_closed(self, tab: TabId, asks: Asks) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::tab_closed(session, tab, asks),
            Mux::Own(session) => session.tab_closed(tab),
        }
    }

    /// Shows `pane` of the session over the other panes of its tab, or no
    /// longer.
    pub fn fullscreen_toggled(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::fullscreen_toggled(session, pane),
            Mux::Own(_) => Err(Failure::Unsupported("show a pane over the others of its tab")),
        }
    }

    /// Stacks `panes` of the session on the first of them, each from whatever
    /// tab it is in. Where there is no room, it stacks nothing, and says
    /// nothing of it.
    pub fn panes_stacked<const N: usize>(self, panes: [PaneId; N]) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::panes_stacked(session, panes),
            Mux::Own(_) => Err(Failure::Unsupported("stack panes")),
        }
    }

    /// Renames the session to `to`, or says why not: a session of the other
    /// multiplexer has it, as each holds names unique among its own alone. A
    /// session with no client takes the name as well as one with.
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

    /// The screen of the terminal `pane` of the session: its lines, in the
    /// colours it shows them in.
    pub fn screen_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::screen_dumped(session, pane),
            Mux::Own(session) => session.dumped(pane, own::api::Dumped::Screen),
        }
    }

    /// The text of the terminal `pane` of the session, its scrollback with it:
    /// a line for each line written to it, however many rows it wraps over,
    /// with no styles and no spaces at its end.
    pub fn text_dumped(self, pane: u32) -> Result<String, Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::text_dumped(session, pane),
            Mux::Own(session) => session.dumped(pane, own::api::Dumped::Text),
        }
    }

    /// Kills the session and lets go of what the multiplexer keeps of it.
    pub fn session_killed(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_killed(session),
            Mux::Own(session) => session.killed(),
        }
    }

    /// Lets go of what the multiplexer keeps of the session, one that ended.
    pub fn session_deleted(self) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::session_deleted(session),
            Mux::Own(_) => Err(Failure::Unsupported("keep a session that ended")),
        }
    }

    /// Takes the client that last typed in the session to its pane `pane`. A
    /// pane already focused is where it was to go.
    pub fn pane_focused(self, pane: PaneId) -> Result<(), Failure> {
        match self {
            Mux::Zellij(session) => zellij::cli::pane_focused(session, pane),
            Mux::Own(session) => {
                let pane = pane.terminal().ok_or(Failure::Unsupported("hold a plugin's pane"))?;
                session.went(own::server::Place::Pane { session: session.id(), pane })
            },
        }
    }

    /// Takes the client that last typed in the session to the terminal `pane`
    /// of `to`, or to `to` alone, on the pane it last had the focus on: in the
    /// session itself, as `pane_focused` does. A pane already focused is where
    /// it was to go.
    pub fn went(self, to: &SessionName, pane: Option<u32>) -> Result<(), Failure> {
        match self {
            Mux::Zellij(_) if own::server::named(to)?.is_some() => {
                Err(Failure::Unsupported(ACROSS))
            },
            Mux::Zellij(at) => zellij::cli::went(at, to, pane),
            Mux::Own(at) => {
                let to = at.named(to)?.ok_or(Failure::Unsupported(ACROSS))?;
                let place = pane.map_or(own::server::Place::Session(to), |pane| {
                    own::server::Place::Pane { session: to, pane }
                });
                at.went(place)
            },
        }
    }

    /// Takes the client that last typed in the session to a session it makes,
    /// `to`, that starts in `cwd`, or where the client started: to one that
    /// runs or ended by that name instead, which it brings back, with `cwd`
    /// unused.
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

/// What an own session cannot do of a pane beside another, as each of its tabs
/// holds one.
const BESIDE: &str = "open a pane beside another";

/// What luneta's client does itself, between an own session and zellij's.
const ACROSS: &str = "take a client between one of zellij's sessions and an own one";

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

/// What `environ`, the variables of a process, sets `name` to: the first that
/// sets it, as `getenv` reads it, where more than one does.
fn value_in<'a>(environ: &'a [OsString], name: &str) -> Option<&'a [u8]> {
    environ.iter().find_map(|entry| {
        entry.as_bytes().strip_prefix(name.as_bytes())?.strip_prefix(b"=")
    })
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
        let session = OwnId::new(3).unwrap();
        assert_eq!(pane_in(&environ(&own)), Some(Named::Own { session, pane: 12 }));
        let zellij = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&[&own[..], &zellij].concat())), Some(named));
        assert_eq!(pane_in(&environ(&["HOME=/h"])), None);
    }

    /// A process whose variables set one twice reads the first, as `getenv`
    /// does, for either multiplexer's.
    #[test]
    fn a_variable_set_twice_reads_as_its_first() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let twice = ["ZELLIJ_SESSION_NAME=work", "ZELLIJ_PANE_ID=2", "ZELLIJ_PANE_ID=5"];
        let named = Named::Zellij(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(pane_in(&environ(&twice)), Some(named));
        let twice = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE=12", "LUNETA_MUX_SESSION=4"];
        let session = OwnId::new(3).unwrap();
        assert_eq!(pane_in(&environ(&twice)), Some(Named::Own { session, pane: 12 }));
    }
}
