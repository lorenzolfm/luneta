//! The multiplexer that holds the sessions of the machine luneta and its
//! daemon run on: what they have it do, what they read of its sessions, and
//! the words that start its client. zellij is the only one, and what luneta
//! does with it goes through `Mux`, and the folders it finds, as `zellij` is
//! private.
//!
//! Not all luneta knows of zellij's ways is behind it yet. The interface
//! speaks zellij's own types, `Laid`, `PaneId`, `Asks` and `Failure`, and
//! `tabs` and `terminal_of` read `Laid` outside it. `panes` parses the JSON
//! `panes_listed` gives, `closes` waits on zellij's sockets and cache as a
//! session it killed settles, and the app keeps the rest, in its `zellij`
//! and `own`.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::watch::Troubles;
use crate::Directory;

mod zellij;

pub use zellij::cli::{tabs, terminal_of, Asks, Client, Failure, Laid, Listed, PaneId, Runs, TabId};
pub use zellij::cli::{PANE_VAR, SESSION_VAR};
pub use zellij::metadata::{CacheFolder, Metadata, Unknown};
pub use zellij::{Exited, Id, Scan, SessionName, Sessions, Socket, SocketFolder, NAME_BYTES_MAX};

/// A multiplexer luneta has its sessions in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mux {
    Zellij,
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

    /// What the multiplexer said of every pane of `session`, in JSON, as
    /// `panes` reads it.
    pub fn panes_listed(self, session: &SessionName) -> Result<Vec<u8>, Failure> {
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

    /// The sessions that ended which the multiplexer keeps to bring back.
    pub fn exited(self) -> Result<Vec<Exited>, String> {
        match self {
            Mux::Zellij => zellij::exited(),
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

    /// The client, attached to `session`, which it makes where none runs, or
    /// without one, in a session of a name the multiplexer makes up; or why
    /// it cannot attach.
    pub fn client_attaching(self, session: Option<&SessionName>) -> Result<ClientWords, String> {
        match self {
            Mux::Zellij => zellij::cli::client_attaching(session).map(zellij::cli::words),
        }
    }

    /// The client that makes `session` in the directory it starts in, and
    /// fails where one runs by that name, or one that ended is kept by it.
    #[must_use]
    pub fn client_making(self, session: &SessionName) -> ClientWords {
        match self {
            Mux::Zellij => zellij::cli::words(zellij::cli::client_making(session)),
        }
    }

    /// The client attached to `session`, which runs, or which ended and comes
    /// back; it makes none. Or why it cannot attach.
    pub fn client_rejoining(self, session: &SessionName) -> Result<ClientWords, String> {
        match self {
            Mux::Zellij => zellij::cli::client_rejoining(session).map(zellij::cli::words),
        }
    }

    /// Whether a client attaches to `session`.
    #[must_use]
    pub fn attaches_to(self, session: &SessionName) -> bool {
        match self {
            Mux::Zellij => zellij::cli::attaches_to(session),
        }
    }

    /// What luneta types into the client before it asks the multiplexer to
    /// move it, and as keys come to luneta, where the multiplexer moves only
    /// a client that typed: none where it would not read it as a key, as
    /// `kitty_keys` says whether the client asked for keys as kitty spells
    /// them.
    #[must_use]
    pub fn nudge(self, kitty_keys: bool) -> Option<&'static [u8]> {
        match self {
            Mux::Zellij => zellij::client::nudge(kitty_keys),
        }
    }

    /// How long an action waits after the client typed, for the multiplexer to
    /// take the key in first.
    #[must_use]
    pub fn nudged(self) -> Duration {
        match self {
            Mux::Zellij => zellij::client::NUDGED,
        }
    }

    /// Whether `said`, what the client left on its terminal as it ended well,
    /// says that the multiplexer dropped it, and the session runs on.
    #[must_use]
    pub fn dropped(self, said: &str) -> bool {
        match self {
            Mux::Zellij => zellij::client::dropped(said),
        }
    }

    /// Where the multiplexer keeps the sockets of its sessions.
    #[must_use]
    pub fn socket_folder(self) -> SocketFolder {
        match self {
            Mux::Zellij => zellij::folder(),
        }
    }

    /// Whether a change at `path`, in the socket folder, can be a session's.
    #[must_use]
    pub fn of_sessions(self, path: &Path) -> bool {
        match self {
            Mux::Zellij => zellij::of_sessions(path),
        }
    }

    /// How long until the youngest of the sessions that `started` so is old
    /// enough to be asked anything, at `now`, where one is too young.
    #[must_use]
    pub fn newborn(
        self,
        started: impl Iterator<Item = SystemTime>,
        now: SystemTime,
    ) -> Option<Duration> {
        match self {
            Mux::Zellij => zellij::newborn(started, now),
        }
    }

    /// Where the multiplexer writes what it keeps of each session: none
    /// without a home to find it in.
    #[must_use]
    pub fn cache_folder(self) -> Option<CacheFolder> {
        match self {
            Mux::Zellij => zellij::metadata::folder(),
        }
    }

    /// Whether a change at `path`, in the cache folder, can be to a session's
    /// metadata.
    #[must_use]
    pub fn is_metadata(self, path: &Path) -> bool {
        match self {
            Mux::Zellij => zellij::metadata::is_metadata(path),
        }
    }

    /// The sessions the multiplexer keeps to bring back from, those that
    /// ended and those that run, by name.
    pub fn kept(self) -> Result<Vec<SessionName>, Unknown> {
        match self {
            Mux::Zellij => zellij::metadata::kept(),
        }
    }

    /// Whether the multiplexer keeps anything of `session` in its cache.
    pub fn cached(self, session: &SessionName) -> Result<bool, Unknown> {
        match self {
            Mux::Zellij => zellij::metadata::cached(session),
        }
    }
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
    /// What the multiplexer last wrote in it of the session `name`: none
    /// before it has.
    pub fn metadata(&self, name: &SessionName) -> io::Result<Option<Metadata>> {
        zellij::metadata::read(self, name)
    }
}
