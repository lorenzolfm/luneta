use std::env;
use std::fmt;
use std::path::PathBuf;

pub fn path(name: &str) -> Result<PathBuf, Unset> {
    let set = |name: &str| env::var_os(name).map(PathBuf::from).filter(|value| value.is_absolute());
    let state = set("XDG_STATE_HOME")
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))
        .ok_or(Unset)?;
    Ok(state.join("luneta").join(name))
}

#[derive(Clone, Copy, Debug)]
pub struct Unset;

impl fmt::Display for Unset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("neither XDG_STATE_HOME nor HOME is set to an absolute path")
    }
}
