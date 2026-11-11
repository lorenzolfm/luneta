//! The variables luneta's own multiplexer gives what runs in its panes, which
//! name the pane by the ids its server gave the pane and its session. It gives
//! neither id to another, so a rename leaves them as they are.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use crate::mux::Named;

/// What names the session of an own pane, by its id, in the variables of what
/// runs in it.
const SESSION_VAR: &str = "LUNETA_MUX_SESSION";

/// What names an own pane, by its id, in the variables of what runs in it.
const PANE_VAR: &str = "LUNETA_MUX_PANE";

/// The variables luneta's own multiplexer gives what runs in its panes.
const PANE_VARS: [&str; 2] = [SESSION_VAR, PANE_VAR];

/// The pane `environ`, the variables of a process, name, where they name
/// both its session and its id.
pub fn pane_in(environ: &[OsString]) -> Option<Named> {
    let id = |name: &str| {
        let value = environ.iter().find_map(|entry| {
            let entry = entry.as_bytes();
            entry.strip_prefix(name.as_bytes())?.strip_prefix(b"=")
        })?;
        std::str::from_utf8(value).ok()?.parse().ok()
    };
    Some(Named::Own { session: id(SESSION_VAR)?, pane: id(PANE_VAR)? })
}

/// Has `command` run without the variables luneta's own multiplexer gives
/// what runs in its panes.
pub fn in_no_pane(command: &mut Command) {
    for name in PANE_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variables name an own pane by both its ids, each a number, and by
    /// neither alone.
    #[test]
    fn the_variables_name_an_own_pane_by_both_its_ids() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let both = environ(&["LUNETA_MUX_SESSION=3", "HOME=/h", "LUNETA_MUX_PANE=12"]);
        assert_eq!(pane_in(&both), Some(Named::Own { session: 3, pane: 12 }));
        let unnamed = [
            environ(&["LUNETA_MUX_SESSION=3"]),
            environ(&["LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=work", "LUNETA_MUX_PANE=12"]),
            environ(&["LUNETA_MUX_SESSION=3", "LUNETA_MUX_PANE="]),
            environ(&["LUNETA_MUX_SESSIONS=3", "LUNETA_MUX_PANE=12"]),
        ];
        for environ in unnamed {
            assert_eq!(pane_in(&environ), None, "{environ:?}");
        }
    }
}
