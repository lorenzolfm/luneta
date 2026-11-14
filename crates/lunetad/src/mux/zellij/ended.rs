use std::time::Duration;

use super::{cli, SessionName};
use crate::log;

const EXITED: &str = "(EXITED - attach to resurrect)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exited {
    pub name: SessionName,
    pub ago: Duration,
}

pub fn exited() -> Result<Vec<Exited>, String> {
    let (exited, left_out) = exited_in(&cli::sessions_listed()?);
    if left_out > 0 {
        log::say(&format!("left out {left_out} sessions zellij keeps whose line does not read"));
    }
    Ok(exited)
}

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
