//! Where luneta keeps what outlives a restart, as the folder the daemon runs
//! in does not: in `luneta` of `$XDG_STATE_HOME`, or of `~/.local/state`
//! where that is not set, on macOS too.

use std::env;
use std::path::PathBuf;

/// Where `name` is kept, by this process's variables: in `luneta` of
/// `$XDG_STATE_HOME`, or of `~/.local/state`. A variable set to nothing is not
/// set, and the spec has a relative `XDG_STATE_HOME` passed over.
#[must_use]
pub fn path(name: &str) -> Option<PathBuf> {
    let set = |name: &str| env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let state = set("XDG_STATE_HOME")
        .filter(|state| state.is_absolute())
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))?;
    Some(state.join("luneta").join(name))
}
