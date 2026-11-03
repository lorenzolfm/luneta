//! The sessions zellij keeps to bring back, which ended, of the machine it
//! runs on, as its CLI lists them, for luneta's picker to list. luneta asks
//! them of its own machine, and a daemon of its machine for a luneta on
//! another, whose picker lists this one's.

use std::process::Command;
use std::time::Duration;

use crate::cli::{quiet, unrun, ANSWER};
use crate::zellij::SessionName;
use crate::{log, program};

/// What `list-sessions` says where no session runs, nor ended to be brought
/// back, as it fails.
const NO_SESSIONS: &str = "No active zellij sessions found.";

/// What follows the name of a session `list-sessions` lists that ended, and
/// that zellij keeps to bring back.
const EXITED: &str = "(EXITED - attach to resurrect)";

/// A session that ended, which zellij keeps to bring back: `switch-session`
/// and `attach` bring it back, and `delete-session` lets it go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exited {
    pub name: SessionName,
    /// How long before zellij listed it it was made, as zellij says: to the
    /// second.
    pub ago: Duration,
}

/// The sessions zellij keeps to bring back, as `list-sessions` lists them
/// now, or why it did not list them, as in time: zellij's CLI waits on every
/// session's server, and one that hangs would hold it up for good. How many
/// `exited_in` left out goes in the log.
pub fn exited() -> Result<Vec<Exited>, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["list-sessions", "--no-formatting"]);
    let ran = program::run(&mut command, ANSWER).map_err(|error| unrun(&error))?;
    if let program::Ran::Ended { stderr, .. } = &ran
        && String::from_utf8_lossy(stderr).contains(NO_SESSIONS)
    {
        return Ok(Vec::new());
    }
    let listed = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    let (exited, left_out) = exited_in(&String::from_utf8_lossy(&listed));
    if left_out > 0 {
        log::say(&format!("left out {left_out} sessions zellij keeps whose line does not read"));
    }
    Ok(exited)
}

/// The sessions that ended among those `listed` lists, a line each, as
/// `list-sessions --no-formatting` lists them: `<name> [Created 1day 2h 3m
/// 4s ago] (EXITED - attach to resurrect)`; and how many of the lines of one
/// that ended it left out, as they do not read so, or are of a name zellij
/// would not take.
fn exited_in(listed: &str) -> (Vec<Exited>, usize) {
    let read = |line: &str| {
        let (name, created) = line.trim_end().rsplit_once(" [Created ")?;
        let ago = created.strip_suffix(" ago]").and_then(spelled_ago)?;
        Some(Exited { name: SessionName::running(name).ok()?, ago })
    };
    let ended: Vec<&str> =
        listed.lines().filter_map(|line| line.trim_end().strip_suffix(EXITED)).collect();
    let exited: Vec<Exited> = ended.iter().copied().filter_map(read).collect();
    let left_out = ended.len() - exited.len();
    (exited, left_out)
}

/// A time as zellij spells one, `1day 2h 3m 4s`, in whole seconds: what it
/// says under a second goes, and a unit it has no word for, nothing.
fn spelled_ago(spelled: &str) -> Option<Duration> {
    let mut secs: u64 = 0;
    for word in spelled.split_whitespace() {
        let digits = word.find(|c: char| !c.is_ascii_digit())?;
        let (count, unit) = word.split_at(digits);
        let each = match unit {
            "years" | "year" => 31_557_600,
            "months" | "month" => 2_630_016,
            "weeks" | "week" | "w" => 604_800,
            "days" | "day" | "d" => 86_400,
            "h" => 3600,
            "m" => 60,
            "s" => 1,
            "ms" | "us" | "ns" => 0,
            _ => return None,
        };
        secs = secs.checked_add(count.parse::<u64>().ok()?.checked_mul(each)?)?;
    }
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sessions that ended are read from what `list-sessions` lists,
    /// with how long ago each was made, to the second; the rest are left
    /// out: those that run, yours, and a line that does not read, which is
    /// counted, as one of a session that ended.
    #[test]
    fn the_sessions_that_ended_are_read_from_what_zellij_lists() {
        let listed = "bipa [Created 1day 5h 24m 9s ago] \n\
                      luneta [Created 1day 5h 20m 52s ago] (current)\n\
                      nixos [Created 1day 11m 50s ago] (EXITED - attach to resurrect)\n\
                      a b [Created 3s ago] (EXITED - attach to resurrect)\n\
                      odd [Created 2fortnights ago] (EXITED - attach to resurrect)\n\
                      (EXITED - attach to resurrect)\n";
        let (exited, left_out) = exited_in(listed);
        let read: Vec<(&str, u64)> =
            exited.iter().map(|exited| (exited.name.as_str(), exited.ago.as_secs())).collect();
        assert_eq!(read, [("nixos", 86_400 + 11 * 60 + 50), ("a b", 3)]);
        assert_eq!(left_out, 2);
    }
}
