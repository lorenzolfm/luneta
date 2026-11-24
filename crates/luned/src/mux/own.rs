use std::ffi::OsString;
use std::num::ParseIntError;
use std::process::Command;
use std::str::FromStr;

pub use lune_protocol::made::Made as OwnMade;

use lune_domain::mux::TerminalKey;

use crate::mux::value_in;

pub mod api;
pub mod frames;
pub mod server;
pub mod viewer;

const TERMINAL_VAR: &str = "LUNE_MUX_TERMINAL";

const TERMINAL_MADE_VAR: &str = "LUNE_MUX_TERMINAL_MADE";

const TERMINAL_VARS: [&str; 2] = [TERMINAL_VAR, TERMINAL_MADE_VAR];

pub(crate) fn terminal_in(environ: &[OsString]) -> Result<Option<TerminalKey>, String> {
    if TERMINAL_VARS.iter().all(|name| value_in(environ, name).is_none()) {
        return Ok(None);
    }
    let key = number_in(environ, TERMINAL_VAR).and_then(|id| {
        let made = number_in(environ, TERMINAL_MADE_VAR)?;
        Ok(TerminalKey { id, made: OwnMade::from_nanos(made) })
    });
    let unnamed =
        |why: String| format!("{TERMINAL_VAR} and {TERMINAL_MADE_VAR} name no terminal: {why}");
    key.map(Some).map_err(unnamed)
}

fn number_in<T>(environ: &[OsString], name: &str) -> Result<T, String>
where
    T: FromStr<Err = ParseIntError>,
{
    let value = value_in(environ, name).ok_or_else(|| format!("{name} is unset"))?;
    let text = String::from_utf8_lossy(value);
    text.parse().map_err(|error| format!("{name}={text:?}: {error}"))
}

pub fn terminal_here() -> Result<Option<TerminalKey>, String> {
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

pub(crate) fn in_no_terminal(command: &mut Command) {
    for name in TERMINAL_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn the_variables_name_a_terminal_by_its_id_and_its_making() {
        let environ = |entries: &[&str]| -> Vec<OsString> {
            entries.iter().map(OsString::from).collect()
        };
        let all = environ(&["LUNE_MUX_TERMINAL=7", "HOME=/h", "LUNE_MUX_TERMINAL_MADE=60"]);
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(terminal_in(&all), Ok(Some(key)));
        assert_eq!(terminal_in(&environ(&["HOME=/h"])), Ok(None));
        let made = "LUNE_MUX_TERMINAL_MADE=60";
        let unnamed = [
            (environ(&[made]), "LUNE_MUX_TERMINAL is unset"),
            (environ(&["LUNE_MUX_TERMINAL=7"]), "LUNE_MUX_TERMINAL_MADE is unset"),
            (
                environ(&["LUNE_MUX_TERMINAL=7", "LUNE_MUX_TERMINAL_MADE=-1"]),
                "LUNE_MUX_TERMINAL_MADE=\"-1\": invalid digit found in string",
            ),
            (
                environ(&["LUNE_MUX_TERMINAL=0", made]),
                "LUNE_MUX_TERMINAL=\"0\": number would be zero for non-zero type",
            ),
            (
                environ(&["LUNE_MUX_TERMINAL=4294967296", made]),
                "LUNE_MUX_TERMINAL=\"4294967296\": number too large to fit in target type",
            ),
            (
                environ(&["LUNE_MUX_TERMINAL=", made]),
                "LUNE_MUX_TERMINAL=\"\": cannot parse integer from empty string",
            ),
        ];
        let no_utf_8 = OsString::from_vec(b"LUNE_MUX_TERMINAL=7\xff".to_vec());
        let no_utf_8 = (
            vec![no_utf_8, OsString::from(made)],
            "LUNE_MUX_TERMINAL=\"7\u{fffd}\": invalid digit found in string",
        );
        for (environ, why) in unnamed.into_iter().chain([no_utf_8]) {
            let unnamed = "LUNE_MUX_TERMINAL and LUNE_MUX_TERMINAL_MADE name no terminal";
            let said = format!("{unnamed}: {why}");
            assert_eq!(terminal_in(&environ), Err(said), "{environ:?}");
        }
    }
}
