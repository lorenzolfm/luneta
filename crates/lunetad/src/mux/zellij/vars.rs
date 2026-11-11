//! The variables zellij gives what runs in its panes, which name the pane:
//! by the name its session had as the pane opened, which a rename leaves
//! behind, and the pane's id there.

use std::env;
use std::ffi::OsString;
use std::process::Command;

use crate::mux::{value_in, Named, PaneVars};
use crate::process;

/// What names the session of a zellij pane, in the variables of what runs in
/// it.
const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";

/// What names the pane a zellij command runs in, in its variables, which
/// zellij opens a pane beside.
pub const PANE_VAR: &str = "ZELLIJ_PANE_ID";

/// What zellij sets in every pane, which its client takes for one started in
/// a pane, as it does what `SESSION_VAR` names.
const IN_A_PANE_VAR: &str = "ZELLIJ";

/// The variables zellij gives what runs in its panes.
pub const PANE_VARS: [&str; 3] = [IN_A_PANE_VAR, SESSION_VAR, PANE_VAR];

/// What the variables of this process say of the pane they name. Or why they
/// do not read.
pub fn here() -> Result<PaneVars, String> {
    let (session, pane) = (process::var(SESSION_VAR)?, process::var(PANE_VAR)?);
    Ok(PaneVars::spelled(session, pane.as_deref()))
}

/// The pane `environ`, the variables of a process, name, if they name one.
#[must_use]
pub fn pane_in(environ: &[OsString]) -> Option<Named> {
    let session = value_in(environ, SESSION_VAR).map(|value| String::from_utf8_lossy(value).into());
    let pane = value_in(environ, PANE_VAR).and_then(|value| std::str::from_utf8(value).ok());
    PaneVars::spelled(session, pane).pane_named().map(Named::Zellij)
}

/// Whether this process may run in a pane of zellij's: what zellij sets in
/// its panes is set, of any value.
pub fn may_be_in_a_pane() -> bool {
    [IN_A_PANE_VAR, SESSION_VAR].iter().any(|name| env::var_os(name).is_some())
}

/// Has `command` run without the variables zellij gives what runs in its
/// panes.
pub fn in_no_pane(command: &mut Command) {
    for name in PANE_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::ZellijPane;

    /// The variables name a session by a name that is not empty, and a pane
    /// by a terminal's id, and a pane they name by both. A name is as they
    /// spell it, which zellij had for its session as the pane opened, or not.
    #[test]
    fn the_variables_name_a_pane_by_its_session_and_a_terminal_s_id() {
        let both = PaneVars::spelled(Some("a/b".to_owned()), Some("2"));
        assert_eq!(both.pane_named(), Some(ZellijPane { session: "a/b".to_owned(), pane: 2 }));
        let some = |text: &str| Some(text.to_owned());
        let halves = [
            (some(""), Some("2"), None, Some(2)),
            (some("work"), Some("terminal_2"), some("work"), None),
            (some("work"), Some(""), some("work"), None),
            (some("work"), None, some("work"), None),
            (None, Some("2"), None, Some(2)),
            (None, None, None, None),
        ];
        for (session, pane, named, id) in halves {
            let here = PaneVars::spelled(session.clone(), pane);
            assert_eq!((here.named(), here.pane()), (named.as_deref(), id), "{session:?} {pane:?}");
            assert_eq!(here.pane_named(), None, "{session:?} {pane:?}");
        }
    }
}
