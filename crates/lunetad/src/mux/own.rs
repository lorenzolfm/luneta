//! luneta's own multiplexer: `luneta-mux`, the one server of every own
//! session of the user, which `server` reaches; and the variables it gives
//! what runs in its panes, which name the pane by the ids its server gave the
//! pane and its session. It gives neither id to another, so a rename leaves
//! them as they are.

use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mux::{value_in, Named};

pub mod api;
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

/// An own session as no other is, of its server or of the next, which counts
/// its ids from 1 again: by the id its server gave it, and when it made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnKey {
    pub id: OwnId,
    pub made: SystemTime,
}

impl OwnKey {
    /// The session `own` says, or why it says none: the id 0, or no time it
    /// was made.
    pub fn of(own: &crate::api::OwnSession) -> Result<Self, String> {
        let id = OwnId::new(own.id).ok_or("own: 0 is no session's id")?;
        let made = own.made_at.ok_or_else(|| format!("own session {id} has no made_at"))?;
        let made = SystemTime::try_from(made)
            .map_err(|why| format!("own session {id}'s made_at: {why}"))?;
        Ok(OwnKey { id, made })
    }
}

impl From<OwnKey> for crate::api::OwnSession {
    fn from(key: OwnKey) -> Self {
        crate::api::OwnSession { id: key.id.get(), made_at: Some(key.made.into()) }
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
    let made = UNIX_EPOCH.checked_add(Duration::from_nanos(text(MADE_VAR)?.parse().ok()?))?;
    let pane = text(PANE_VAR)?.parse().ok().filter(|pane| *pane != 0)?;
    Some(Named::Own { session: OwnKey { id, made }, pane })
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
        let made = UNIX_EPOCH + Duration::from_secs(60) + Duration::from_nanos(5);
        let session = OwnKey { id: OwnId::new(3).unwrap(), made };
        assert_eq!(pane_in(&all), Some(Named::Own { session, pane: 12 }));
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
}
