use std::ffi::OsString;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mux::value_in;

pub mod api;
pub mod attach;
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

impl OwnKey {
    pub fn of(own: &crate::api::OwnSession) -> Result<Self, String> {
        let id = OwnId::new(own.id).ok_or("own: 0 is no session's id")?;
        let made = own.made_at.ok_or_else(|| format!("own session {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("own session {id}'s made_at: {why}"))?;
        Ok(OwnKey { id, made })
    }
}

impl From<TerminalKey> for crate::api::TerminalKey {
    fn from(key: TerminalKey) -> Self {
        crate::api::TerminalKey { id: key.id.get(), made_at: Some(key.made.into()) }
    }
}

impl From<OwnKey> for crate::api::OwnSession {
    fn from(key: OwnKey) -> Self {
        crate::api::OwnSession { id: key.id.get(), made_at: Some(key.made.into()) }
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

const SESSION_VAR: &str = "LUNETA_MUX_SESSION";

const MADE_VAR: &str = "LUNETA_MUX_MADE";

const PANE_VAR: &str = "LUNETA_MUX_PANE";

const PANE_VARS: [&str; 3] = [SESSION_VAR, MADE_VAR, PANE_VAR];

const TERMINAL_VAR: &str = "LUNETA_MUX_TERMINAL";

const TERMINAL_MADE_VAR: &str = "LUNETA_MUX_TERMINAL_MADE";

const TERMINAL_VARS: [&str; 2] = [TERMINAL_VAR, TERMINAL_MADE_VAR];

pub(crate) fn terminal_in(environ: &[OsString]) -> Option<TerminalKey> {
    let text = |name: &str| std::str::from_utf8(value_in(environ, name)?).ok();
    let id = NonZeroU32::new(text(TERMINAL_VAR)?.parse().ok()?)?;
    let made = OwnMade::from_nanos(text(TERMINAL_MADE_VAR)?.parse().ok()?);
    Some(TerminalKey { id, made })
}

pub(crate) fn pane_in(environ: &[OsString]) -> Option<OwnPane> {
    let text = |name: &str| std::str::from_utf8(value_in(environ, name)?).ok();
    let id = OwnId::new(text(SESSION_VAR)?.parse().ok()?)?;
    let made = OwnMade::from_nanos(text(MADE_VAR)?.parse().ok()?);
    let pane = text(PANE_VAR)?.parse().ok()?;
    Some(OwnPane { session: OwnKey { id, made }, id: pane })
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

pub fn here() -> Result<Option<OwnPane>, String> {
    let environ: Vec<OsString> = std::env::vars_os()
        .map(|(name, value)| {
            let mut entry = name;
            entry.push("=");
            entry.push(value);
            entry
        })
        .collect();
    here_in(&environ)
}

fn here_in(environ: &[OsString]) -> Result<Option<OwnPane>, String> {
    if PANE_VARS.iter().all(|name| value_in(environ, name).is_none()) {
        return Ok(None);
    }
    let unread =
        || format!("{SESSION_VAR}, {MADE_VAR} and {PANE_VAR} name no pane of luneta-mux's");
    pane_in(environ).map(Some).ok_or_else(unread)
}

pub(crate) fn in_no_pane(command: &mut Command) {
    for name in PANE_VARS.into_iter().chain(TERMINAL_VARS) {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variables_name_an_own_pane_by_its_ids_and_its_session_s_making() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let all = ["LUNETA_MUX_SESSION=3", "HOME=/h", "LUNETA_MUX_MADE=60000000005"];
        let all = environ(&[&all[..], &["LUNETA_MUX_PANE=12"]].concat());
        let made = OwnMade::from_nanos(60_000_000_005);
        let session = OwnKey { id: OwnId::new(3).unwrap(), made };
        assert_eq!(pane_in(&all), OwnPane::new(session, 12));
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

    #[test]
    fn a_pane_s_variables_set_that_do_not_read_are_no_pane_and_not_none() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        assert_eq!(here_in(&environ(&["HOME=/h"])), Ok(None));
        let all = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        let session = OwnKey { id: OwnId::new(3).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(here_in(&environ(&all)), Ok(OwnPane::new(session, 12)));
        let unread = "LUNETA_MUX_SESSION, LUNETA_MUX_MADE and LUNETA_MUX_PANE name no pane of \
                      luneta-mux's";
        for partial in [
            environ(&all[..2]),
            environ(&all[1..]),
            environ(&[all[0], all[2]]),
            environ(&["LUNETA_MUX_MADE="]),
            environ(&["LUNETA_MUX_SESSION=work", all[1], all[2]]),
        ] {
            assert_eq!(here_in(&partial), Err(unread.to_owned()), "{partial:?}");
        }
    }

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
