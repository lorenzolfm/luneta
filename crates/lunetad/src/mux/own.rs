use std::ffi::OsString;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mux::value_in;

pub mod api;
pub mod frames;
pub mod server;
pub mod viewer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnId(NonZeroU64);

impl OwnId {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnClientId(NonZeroU64);

impl OwnClientId {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnMade(u64);

impl OwnMade {
    #[must_use]
    pub fn of(at: SystemTime) -> Option<Self> {
        u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok().map(OwnMade)
    }

    #[must_use]
    pub fn from_nanos(nanos: u64) -> Self {
        OwnMade(nanos)
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalKey {
    pub id: NonZeroU32,
    pub made: OwnMade,
}

impl TerminalKey {
    pub fn read(key: Option<api::TerminalKey>) -> Result<Self, String> {
        let key = key.ok_or("a terminal with no key")?;
        let id = NonZeroU32::new(key.id).ok_or("0 is no terminal's id")?;
        let made = key.made_at.ok_or_else(|| format!("terminal {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("terminal {id}'s made_at: {why}"))?;
        Ok(TerminalKey { id, made })
    }

    pub fn of(key: &crate::api::TerminalKey) -> Result<Self, String> {
        let id = NonZeroU32::new(key.id).ok_or("0 is no terminal's id")?;
        let made = key.made_at.ok_or_else(|| format!("terminal {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("terminal {id}'s made_at: {why}"))?;
        Ok(TerminalKey { id, made })
    }

    #[must_use]
    pub fn said(self) -> api::TerminalKey {
        api::TerminalKey { id: self.id.get(), made_at: Some(self.made.into()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnKey {
    pub id: OwnId,
    pub made: OwnMade,
}

impl From<TerminalKey> for crate::api::TerminalKey {
    fn from(key: TerminalKey) -> Self {
        crate::api::TerminalKey { id: key.id.get(), made_at: Some(key.made.into()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnPane {
    pub session: OwnKey,
    pub id: NonZeroU32,
}

impl OwnPane {
    #[must_use]
    pub fn new(session: OwnKey, pane: u32) -> Option<Self> {
        NonZeroU32::new(pane).map(|id| OwnPane { session, id })
    }
}

const TERMINAL_VAR: &str = "LUNETA_MUX_TERMINAL";

const TERMINAL_MADE_VAR: &str = "LUNETA_MUX_TERMINAL_MADE";

const TERMINAL_VARS: [&str; 2] = [TERMINAL_VAR, TERMINAL_MADE_VAR];

pub(crate) fn terminal_in(environ: &[OsString]) -> Option<TerminalKey> {
    let text = |name: &str| std::str::from_utf8(value_in(environ, name)?).ok();
    let id = NonZeroU32::new(text(TERMINAL_VAR)?.parse().ok()?)?;
    let made = OwnMade::from_nanos(text(TERMINAL_MADE_VAR)?.parse().ok()?);
    Some(TerminalKey { id, made })
}

#[must_use]
pub fn terminal_here() -> Option<TerminalKey> {
    terminal_in(&environ())
}

fn environ() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(name, value)| {
            let mut entry = name;
            entry.push("=");
            entry.push(value);
            entry
        })
        .collect()
}

pub(crate) fn in_no_pane(command: &mut Command) {
    for name in TERMINAL_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variables_name_a_terminal_by_its_id_and_its_making() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let all = environ(&["LUNETA_MUX_TERMINAL=7", "HOME=/h", "LUNETA_MUX_TERMINAL_MADE=60"]);
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(terminal_in(&all), Some(key));
        let made = "LUNETA_MUX_TERMINAL_MADE=60";
        let unnamed = [
            environ(&[made]),
            environ(&["LUNETA_MUX_TERMINAL=7"]),
            environ(&["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=-1"]),
            environ(&["LUNETA_MUX_TERMINAL=0", made]),
            environ(&["LUNETA_MUX_TERMINAL=4294967296", made]),
            environ(&["LUNETA_MUX_TERMINAL=", made]),
        ];
        for environ in unnamed {
            assert_eq!(terminal_in(&environ), None, "{environ:?}");
        }
    }
}
