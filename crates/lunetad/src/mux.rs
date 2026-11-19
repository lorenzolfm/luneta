use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use crate::process;

pub mod own;

pub use own::{OwnClientId, OwnId, OwnKey, OwnMade, OwnPane, TerminalKey};

#[derive(Debug)]
pub enum Failure {
    Own(String),
    Unread(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Own(said) => write!(f, "luneta-mux: {said}"),
            Failure::Unread(why) => f.write_str(why),
        }
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs<'a> {
    Shell(&'a OsStr),
    Command { program: &'a str, args: &'a [String] },
}

#[must_use]
pub fn pane_of(pid: u32) -> Option<TerminalKey> {
    own::terminal_in(&process::environ(pid)?)
}

fn value_in<'a>(environ: &'a [OsString], name: &str) -> Option<&'a [u8]> {
    environ.iter().find_map(|entry| {
        entry.as_bytes().strip_prefix(name.as_bytes())?.strip_prefix(b"=")
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn environ(entries: &[&str]) -> Vec<OsString> {
        entries.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_terminal_is_named_by_its_variables() {
        let terminal = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(own::terminal_in(&environ(&terminal)), Some(key));
        assert_eq!(own::terminal_in(&environ(&["HOME=/h"])), None);
        let old = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        assert_eq!(own::terminal_in(&environ(&old)), None, "an own session's pane is no pane now");
    }

    #[test]
    fn a_variable_set_twice_reads_as_its_first() {
        let twice = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let twice = [&twice[..], &["LUNETA_MUX_TERMINAL=8"]].concat();
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(own::terminal_in(&environ(&twice)), Some(key));
    }
}
