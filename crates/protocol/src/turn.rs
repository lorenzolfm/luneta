use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::process::Command;

pub const TURN_VAR: &str = "LUNE_TURN";
pub const BEFORE_VAR: &str = "LUNE_TURN_BEFORE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    key: u64,
    before: Option<u64>,
}

impl Turn {
    #[must_use]
    pub fn after(before: Option<u64>) -> Turn {
        let key = RandomState::new().build_hasher().finish();
        let key = if before == Some(key) { !key } else { key };
        Turn { key, before }
    }

    pub fn new(key: u64, before: Option<u64>) -> Result<Turn, String> {
        if before == Some(key) {
            return Err(format!(
                "turn: {key:016x} is the key of the turn before it too"
            ));
        }
        Ok(Turn { key, before })
    }

    #[must_use]
    pub fn key(self) -> u64 {
        self.key
    }

    #[must_use]
    pub fn before(self) -> Option<u64> {
        self.before
    }

    pub fn name_in(self, command: &mut Command) {
        command.env(TURN_VAR, format!("{:016x}", self.key));
        match self.before {
            Some(before) => command.env(BEFORE_VAR, format!("{before:016x}")),
            None => command.env_remove(BEFORE_VAR),
        };
    }

    pub fn read(key: Option<&str>, before: Option<&str>) -> Result<Option<Turn>, String> {
        let read = |name, spelled: &str| {
            let read = u64::from_str_radix(spelled, 16);
            read.map_err(|_| format!("{name} is no key of a turn of lune's ask box"))
        };
        match (key, before) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(format!("{BEFORE_VAR} is set, and {TURN_VAR} is not")),
            (Some(key), before) => {
                let before = before.map(|before| read(BEFORE_VAR, before)).transpose()?;
                Turn::new(read(TURN_VAR, key)?, before).map(Some)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_goes_by_its_own_key_and_the_one_before() {
        let first = Turn::after(None);
        let second = Turn::after(Some(first.key()));
        let third = Turn::after(Some(second.key()));
        let befores = [first.before(), second.before(), third.before()];
        assert_eq!(befores, [None, Some(first.key()), Some(second.key())]);
        assert!(first.key() != second.key() && second.key() != third.key());
    }

    #[test]
    fn the_agent_s_commands_read_no_turn_the_box_could_not_name() {
        assert_eq!(
            Turn::read(Some("ff"), Some("0a")),
            Ok(Some(Turn {
                key: 255,
                before: Some(10)
            }))
        );
        for (key, before) in [
            (Some(""), None),
            (Some("forged"), None),
            (Some("ff"), Some("")),
            (None, Some("ff")),
            (Some("ff"), Some("ff")),
        ] {
            assert!(Turn::read(key, before).is_err(), "{key:?} {before:?}");
        }
    }
}
