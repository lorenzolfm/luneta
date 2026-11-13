//! luneta's own multiplexer: `luneta-mux`, the one server of every own
//! session of the user, which `server` reaches; and the variables it gives
//! what runs in its panes, which name the pane by the ids its server gave the
//! pane and its session. It gives neither id to another, so a rename leaves
//! them as they are.

use std::ffi::OsString;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mux::{value_in, Named};

pub mod api;
pub mod attach;
pub mod server;

/// An own session, by the id its server gave it, which it gives no other, and
/// which a rename keeps. Never 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnId(NonZeroU64);

impl OwnId {
    /// The session `id` names, or none: 0 names none.
    #[must_use]
    pub fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(OwnId)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for OwnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A client of the own sessions, by the id its server gave it, which it gives
/// no other client: luneta's, which a go its ask box asks for takes. Never 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnClientId(NonZeroU64);

impl OwnClientId {
    /// The client `id` names, or none: 0 names none.
    #[must_use]
    pub fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(OwnClientId)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for OwnClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// When an own session was made, in nanoseconds since the epoch, as its
/// server names it in the variables of its panes: never before 1970, nor
/// past 2554.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnMade(u64);

impl OwnMade {
    /// The moment `at`, or none before 1970 or past 2554.
    #[must_use]
    pub fn of(at: SystemTime) -> Option<Self> {
        u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok().map(OwnMade)
    }

    /// The moment `nanos` nanoseconds after the epoch.
    #[must_use]
    pub fn from_nanos(nanos: u64) -> Self {
        OwnMade(nanos)
    }

    /// The moment `at` says, or why it is none of an own session's.
    pub fn read(at: prost_types::Timestamp) -> Result<Self, String> {
        let at = SystemTime::try_from(at).map_err(|why| why.to_string())?;
        OwnMade::of(at).ok_or_else(|| "a time before 1970, or past 2554".to_owned())
    }

    #[must_use]
    pub fn nanos(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn at(self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.0)
    }
}

impl From<OwnMade> for prost_types::Timestamp {
    fn from(made: OwnMade) -> Self {
        made.at().into()
    }
}

/// An own session as no other is, of its server or of the next, which counts
/// its ids from 1 again: by the id its server gave it, and when it made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnKey {
    pub id: OwnId,
    pub made: OwnMade,
}

impl OwnKey {
    /// The session `own` says, or why it says none: the id 0, or no time it
    /// was made that an own session's is.
    pub fn of(own: &crate::api::OwnSession) -> Result<Self, String> {
        let id = OwnId::new(own.id).ok_or("own: 0 is no session's id")?;
        let made = own.made_at.ok_or_else(|| format!("own session {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("own session {id}'s made_at: {why}"))?;
        Ok(OwnKey { id, made })
    }
}

impl From<OwnKey> for crate::api::OwnSession {
    fn from(key: OwnKey) -> Self {
        crate::api::OwnSession { id: key.id.get(), made_at: Some(key.made.into()) }
    }
}

/// A pane of an own session, as no other is: by its session's key, and the id
/// its server gave it, which it gives no other pane of its sessions. Never 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnPane {
    pub session: OwnKey,
    pub id: NonZeroU32,
}

impl OwnPane {
    /// The pane `pane` of `session`, or none: 0 names none.
    #[must_use]
    pub fn new(session: OwnKey, pane: u32) -> Option<Self> {
        NonZeroU32::new(pane).map(|id| OwnPane { session, id })
    }

    /// The pane `own` says, or why it says none: no session, or one that does
    /// not read, or the pane 0.
    pub fn of(own: &crate::api::Own) -> Result<Self, String> {
        let pane = own.pane;
        let none = || format!("own pane {pane} of no session");
        let session = own.session.as_ref().ok_or_else(none)?;
        let session = OwnKey::of(session).map_err(|why| format!("own pane {pane}: {why}"))?;
        let zero = || format!("own session {}: 0 is no pane's id", session.id);
        OwnPane::new(session, pane).ok_or_else(zero)
    }
}

impl From<OwnPane> for crate::api::Own {
    fn from(own: OwnPane) -> Self {
        crate::api::Own { session: Some(own.session.into()), pane: own.id.get() }
    }
}

/// What names the session of an own pane, by its id, in the variables of what
/// runs in it.
const SESSION_VAR: &str = "LUNETA_MUX_SESSION";

/// What says when the session of an own pane was made, in nanoseconds since
/// the epoch, in the variables of what runs in it.
const MADE_VAR: &str = "LUNETA_MUX_MADE";

/// What names an own pane, by its id, in the variables of what runs in it.
const PANE_VAR: &str = "LUNETA_MUX_PANE";

/// The variables luneta's own multiplexer gives what runs in its panes.
const PANE_VARS: [&str; 3] = [SESSION_VAR, MADE_VAR, PANE_VAR];

/// The pane `environ`, the variables of a process, name, where they name
/// its session, when that was made, and its id.
pub(crate) fn pane_in(environ: &[OsString]) -> Option<Named> {
    let text = |name: &str| std::str::from_utf8(value_in(environ, name)?).ok();
    let id = OwnId::new(text(SESSION_VAR)?.parse().ok()?)?;
    let made = OwnMade::from_nanos(text(MADE_VAR)?.parse().ok()?);
    let pane = text(PANE_VAR)?.parse().ok()?;
    Some(Named::Own(OwnPane { session: OwnKey { id, made }, id: pane }))
}

/// The own pane this process runs in, as its variables name it, with its
/// session: none where it runs in none.
#[must_use]
pub fn here() -> Option<OwnPane> {
    let environ: Vec<OsString> = std::env::vars_os()
        .map(|(name, value)| {
            let mut entry = name;
            entry.push("=");
            entry.push(value);
            entry
        })
        .collect();
    match pane_in(&environ)? {
        Named::Own(own) => Some(own),
        Named::Zellij(_) => None,
    }
}

/// Has `command` run without the variables luneta's own multiplexer gives
/// what runs in its panes.
pub(crate) fn in_no_pane(command: &mut Command) {
    for name in PANE_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variables name an own pane by its ids, each a number and not 0,
    /// and by when its session was made, and by none of them alone.
    #[test]
    fn the_variables_name_an_own_pane_by_its_ids_and_its_session_s_making() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let all = ["LUNETA_MUX_SESSION=3", "HOME=/h", "LUNETA_MUX_MADE=60000000005"];
        let all = environ(&[&all[..], &["LUNETA_MUX_PANE=12"]].concat());
        let made = OwnMade::from_nanos(60_000_000_005);
        let session = OwnKey { id: OwnId::new(3).unwrap(), made };
        assert_eq!(pane_in(&all), OwnPane::new(session, 12).map(Named::Own));
        let made = "LUNETA_MUX_MADE=60";
        let unnamed = [
            environ(&["LUNETA_MUX_SESSION=3", made]),
            environ(&["LUNETA_MUX_PANE=12", made]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=-1", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=work", made, "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", made, "LUNETA_MUX_PANE="]),
            environ(&["LUNETA_MUX_SESSION=3", made, "LUNETA_MUX_PANE=4294967296"]),
            environ(&["LUNETA_MUX_SESSION=0", made, "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", made, "LUNETA_MUX_PANE=0"]),
            environ(&["LUNETA_MUX_SESSIONS=3", made, "LUNETA_MUX_PANE=12"]),
        ];
        for environ in unnamed {
            assert_eq!(pane_in(&environ), None, "{environ:?}");
        }
    }

    /// An own pane, as the daemon says it, is of a session that reads, made
    /// after 1970, and never of the id 0; and it is said as it reads.
    #[test]
    fn an_own_pane_said_reads_as_one_of_a_session_and_not_0() {
        use crate::api::{Own, OwnSession};
        let made = OwnMade::from_nanos(60_000_000_000);
        let pane = OwnPane::new(OwnKey { id: OwnId::new(3).unwrap(), made }, 12).unwrap();
        let said = Own::from(pane);
        assert_eq!(OwnPane::of(&said), Ok(pane));
        let session = said.session.unwrap();
        let before_1970 = prost_types::Timestamp { seconds: -1, nanos: 0 };
        let unread = [
            Own { pane: 0, ..said },
            Own { session: None, ..said },
            Own { session: Some(OwnSession { id: 0, ..session }), ..said },
            Own { session: Some(OwnSession { made_at: None, ..session }), ..said },
            Own { session: Some(OwnSession { made_at: Some(before_1970), ..session }), ..said },
        ];
        for own in unread {
            assert!(OwnPane::of(&own).is_err(), "{own:?}");
        }
    }
}
