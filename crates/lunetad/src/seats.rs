use std::collections::HashMap;
use std::num::NonZeroU32;

use luneta_protocol::unread::Unplaced;

pub use crate::agent::Unread;
use crate::api::{Agent, UnreadFile};
use crate::mux::TerminalKey;

#[must_use]
pub fn seated(agents: &[Agent]) -> HashMap<TerminalKey, &Agent> {
    let mut seated: HashMap<TerminalKey, &Agent> = HashMap::new();
    for agent in agents {
        let Some(seat) = agent.terminal.as_ref().and_then(|key| TerminalKey::of(key).ok()) else {
            continue;
        };
        let rank = |agent: &Agent| {
            let started = agent.started_at.as_ref().map(|at| (at.seconds, at.nanos));
            (!agent.at_a_prompt(), started)
        };
        seated
            .entry(seat)
            .and_modify(|seat| {
                if rank(agent) < rank(seat) {
                    *seat = agent;
                }
            })
            .or_insert(agent);
    }
    seated
}

pub fn unread(told: u32, files: &[UnreadFile]) -> Result<Vec<Unread>, Unplaced> {
    let listed = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let unread_count = told.max(listed);
    let mut unplaced = unread_count - listed;
    let mut placed = Vec::with_capacity(files.len());
    for file in files {
        match file.terminal.as_ref().map(TerminalKey::of).transpose() {
            Ok(pane) => placed.push(Unread { pid: file.pid, pane }),
            Err(_) => unplaced += 1,
        }
    }
    let Some(unplaced) = NonZeroU32::new(unplaced) else {
        return Ok(placed);
    };
    let unread_count =
        NonZeroU32::new(unread_count).expect("a file it cannot place is one it cannot read");
    Err(Unplaced::new(unread_count, unplaced))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api;
    use crate::mux::OwnMade;

    fn file(pid: u32) -> UnreadFile {
        UnreadFile { pid, terminal: None }
    }

    fn unplaced(unread: u32, unplaced: u32) -> Unplaced {
        let count = |count| NonZeroU32::new(count).unwrap();
        Unplaced::new(count(unread), count(unplaced))
    }

    fn key(id: u32, made: u64) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(made) }
    }

    #[test]
    fn a_file_is_where_its_process_runs_unless_lunetad_cannot_follow_it() {
        let said = api::TerminalKey::from(key(3, 60));
        let placed = [UnreadFile { terminal: Some(said), ..file(10) }, file(11)];
        let seats = vec![Unread { pid: 10, pane: Some(key(3, 60)) }, Unread { pid: 11, pane: None }];
        assert_eq!(unread(2, &placed), Ok(seats));
        assert_eq!(unread(0, &[]), Ok(Vec::new()));

        let lost = [UnreadFile { terminal: Some(api::TerminalKey { id: 0, ..said }), ..file(10) }];
        assert_eq!(unread(1, &lost), Err(unplaced(1, 1)));
        assert_eq!(unread(3, &[]), Err(unplaced(3, 3)));
        assert_eq!(unread(3, &lost), Err(unplaced(3, 3)));
    }

    #[test]
    fn a_file_in_a_terminal_is_in_the_terminal_of_its_key() {
        let in_terminal =
            |key: api::TerminalKey| UnreadFile { terminal: Some(key), ..file(10) };
        let said = api::TerminalKey::from(key(7, 60));
        let placed = [in_terminal(said)];
        assert_eq!(unread(1, &placed), Ok(vec![Unread { pid: 10, pane: Some(key(7, 60)) }]));

        let zero = in_terminal(api::TerminalKey { id: 0, ..said });
        let unmade = in_terminal(api::TerminalKey { made_at: None, ..said });
        for unplaceable in [zero, unmade] {
            let files = std::slice::from_ref(&unplaceable);
            assert_eq!(unread(1, files), Err(unplaced(1, 1)), "{unplaceable:?}");
        }
    }

    #[test]
    fn an_agent_sits_in_the_terminal_its_whole_key_names() {
        let in_terminal = |pid: u32, key: TerminalKey| Agent {
            pid,
            terminal: Some(key.into()),
            ..Agent::default()
        };
        let agents = [in_terminal(10, key(3, 60)), in_terminal(11, key(3, 70)), unkeyed(12)];
        let seated = seated(&agents);
        let pids = |key| seated.get(&key).map(|agent| agent.pid);
        assert_eq!((pids(key(3, 60)), pids(key(3, 70)), seated.len()), (Some(10), Some(11), 2));
    }

    fn unkeyed(pid: u32) -> Agent {
        let zero = api::TerminalKey { id: 0, made_at: None };
        Agent { pid, terminal: Some(zero), ..Agent::default() }
    }
}
