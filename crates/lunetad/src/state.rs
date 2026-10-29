//! Where luneta keeps what outlives a restart, as the folder the daemon runs
//! in does not: in `luneta` of `$XDG_STATE_HOME`, or of `~/.local/state`
//! where that is not set, on macOS too.

use std::env;
use std::fmt;
use std::path::PathBuf;

/// Where `name` is kept, by this process's variables: in `luneta` of
/// `$XDG_STATE_HOME`, or of `~/.local/state`. A variable set to nothing, or
/// to a relative path, is not set: the spec has a relative `XDG_STATE_HOME`
/// passed over, and a relative `HOME` would put the log of lunetad, which
/// runs in `/`, elsewhere than luneta's.
pub fn path(name: &str) -> Result<PathBuf, Unset> {
    let set = |name: &str| env::var_os(name).map(PathBuf::from).filter(|value| value.is_absolute());
    let state = set("XDG_STATE_HOME")
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))
        .ok_or(Unset)?;
    Ok(state.join("luneta").join(name))
}

/// Why there is nowhere to keep luneta's state.
#[derive(Clone, Copy, Debug)]
pub struct Unset;

impl fmt::Display for Unset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("neither XDG_STATE_HOME nor HOME is set to an absolute path")
    }
}
