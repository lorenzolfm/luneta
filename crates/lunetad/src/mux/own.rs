//! luneta's own multiplexer: `luneta-mux`, the one server of every own
//! session of the user, which `server` reaches; and the variables it gives
//! what runs in its panes, which name the pane by the ids its server gave the
//! pane and its session. It gives neither id to another, so a rename leaves
//! them as they are.

use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::process::Command;

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

/// What names the session of an own pane, by its id, in the variables of what
/// runs in it.
const SESSION_VAR: &str = "LUNETA_MUX_SESSION";

/// What names an own pane, by its id, in the variables of what runs in it.
const PANE_VAR: &str = "LUNETA_MUX_PANE";

/// The variables luneta's own multiplexer gives what runs in its panes.
const PANE_VARS: [&str; 2] = [SESSION_VAR, PANE_VAR];

/// The pane `environ`, the variables of a process, name, where they name
/// both its session and its id.
pub(crate) fn pane_in(environ: &[OsString]) -> Option<Named> {
    let text = |name: &str| std::str::from_utf8(value_in(environ, name)?).ok();
    let session = OwnId::new(text(SESSION_VAR)?.parse().ok()?)?;
    let pane = text(PANE_VAR)?.parse().ok().filter(|pane| *pane != 0)?;
    Some(Named::Own { session, pane })
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

    /// The variables name an own pane by both its ids, each a number and
    /// not 0, and by neither alone.
    #[test]
    fn the_variables_name_an_own_pane_by_both_its_ids() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let both = environ(&["LUNETA_MUX_SESSION=3", "HOME=/h", "LUNETA_MUX_PANE=12"]);
        let session = OwnId::new(3).unwrap();
        assert_eq!(pane_in(&both), Some(Named::Own { session, pane: 12 }));
        let unnamed = [
            environ(&["LUNETA_MUX_SESSION=3"]),
            environ(&["LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=work", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE="]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE=4294967296"]),
            environ(&["LUNETA_MUX_SESSION=0", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE=0"]),
            environ(&["LUNETA_MUX_SESSIONS=3", "LUNETA_MUX_PANE=12"]),
        ];
        for environ in unnamed {
            assert_eq!(pane_in(&environ), None, "{environ:?}");
        }
    }
}
