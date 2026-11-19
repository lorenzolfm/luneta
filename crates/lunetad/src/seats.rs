use std::collections::HashMap;
use std::num::NonZeroU32;

use luneta_protocol::unread::Unplaced;

use crate::api::{agent, Agent, UnreadFile};
use crate::mux::{OwnKey, OwnMade, OwnPane, TerminalKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Where {
    Own(OwnKey),
    Terminal(OwnMade),
}

impl Where {
    #[must_use]
    pub fn of_terminal(key: TerminalKey) -> (Where, u32) {
        (Where::Terminal(key.made), key.id.get())
    }
}

#[must_use]
pub fn seated(agents: &[Agent]) -> HashMap<(Where, u32), &Agent> {
    let mut seated: HashMap<(Where, u32), &Agent> = HashMap::new();
    for agent in agents {
        let seat = match &agent.pane {
            Some(agent::Pane::Own(own)) => {
                OwnPane::of(own).ok().map(|own| (Where::Own(own.session), own.id.get()))
            },
            Some(agent::Pane::Terminal(key)) => TerminalKey::of(key).ok().map(Where::of_terminal),
            None => None,
        };
        let Some(seat) = seat else {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadSeat {
    NoPane,
    Own(OwnPane),
    Terminal(TerminalKey),
    Unplaced,
}

impl UnreadSeat {
    #[must_use]
    pub fn of(file: &UnreadFile) -> Self {
        match (&file.own, &file.terminal) {
            (None, None) => UnreadSeat::NoPane,
            (Some(own), None) => OwnPane::of(own).map_or(UnreadSeat::Unplaced, UnreadSeat::Own),
            (None, Some(key)) => {
                TerminalKey::of(key).map_or(UnreadSeat::Unplaced, UnreadSeat::Terminal)
            },
            (Some(_), Some(_)) => UnreadSeat::Unplaced,
        }
    }
}

pub fn unread(told: u32, files: &[UnreadFile]) -> Result<Vec<(Where, u32)>, Unplaced> {
    let listed = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let unread_count = told.max(listed);
    let mut unplaced = unread_count - listed;
    let mut seats = Vec::with_capacity(files.len());
    for file in files {
        match UnreadSeat::of(file) {
            UnreadSeat::NoPane => {},
            UnreadSeat::Own(own) => seats.push((Where::Own(own.session), own.id.get())),
            UnreadSeat::Terminal(key) => seats.push(Where::of_terminal(key)),
            UnreadSeat::Unplaced => unplaced += 1,
        }
    }
    let Some(unplaced) = NonZeroU32::new(unplaced) else {
        return Ok(seats);
    };
    let unread_count =
        NonZeroU32::new(unread_count).expect("a file it cannot place is one it cannot read");
    Err(Unplaced::new(unread_count, unplaced))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api;
    use crate::mux::{OwnId, OwnMade};

    fn file(pid: u32) -> UnreadFile {
        UnreadFile { pid, own: None, terminal: None }
    }

    fn unplaced(unread: u32, unplaced: u32) -> Unplaced {
        let count = |count| NonZeroU32::new(count).unwrap();
        Unplaced::new(count(unread), count(unplaced))
    }

    fn key(id: u64) -> OwnKey {
        OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(60) }
    }

    #[test]
    fn a_file_is_where_its_process_runs_unless_lunetad_cannot_follow_it() {
        let id = NonZeroU32::new(3).unwrap();
        let key = api::TerminalKey::from(TerminalKey { id, made: OwnMade::from_nanos(60) });
        let placed = [UnreadFile { terminal: Some(key), ..file(10) }, file(11)];
        assert_eq!(unread(2, &placed), Ok(vec![(Where::Terminal(OwnMade::from_nanos(60)), 3)]));
        assert_eq!(unread(0, &[]), Ok(Vec::new()));

        let lost = [UnreadFile { terminal: Some(api::TerminalKey { id: 0, ..key }), ..file(10) }];
        assert_eq!(unread(1, &lost), Err(unplaced(1, 1)));
        assert_eq!(unread(3, &[]), Err(unplaced(3, 3)));
        assert_eq!(unread(3, &lost), Err(unplaced(3, 3)));
    }

    #[test]
    fn a_file_in_an_own_pane_is_in_its_own_session() {
        let in_own = |own: api::Own| UnreadFile { own: Some(own), ..file(10) };
        let pane = api::Own::from(OwnPane::new(key(3), 12).unwrap());
        assert_eq!(unread(1, &[in_own(pane)]), Ok(vec![(Where::Own(key(3)), 12)]));

        let zero = in_own(api::Own { pane: 0, ..pane });
        let unkeyed = in_own(api::Own { session: None, ..pane });
        for unplaceable in [zero, unkeyed] {
            let files = std::slice::from_ref(&unplaceable);
            assert_eq!(unread(1, files), Err(unplaced(1, 1)), "{unplaceable:?}");
        }
    }

    #[test]
    fn a_file_in_a_terminal_is_in_the_terminal_of_its_key() {
        let in_terminal =
            |key: api::TerminalKey| UnreadFile { terminal: Some(key), ..file(10) };
        let id = NonZeroU32::new(7).unwrap();
        let key = TerminalKey { id, made: OwnMade::from_nanos(60) };
        let said = api::TerminalKey::from(key);
        let placed = [in_terminal(said)];
        let seats = unread(1, &placed);
        assert_eq!(seats, Ok(vec![(Where::Terminal(OwnMade::from_nanos(60)), 7)]));

        let zero = in_terminal(api::TerminalKey { id: 0, ..said });
        let unmade = in_terminal(api::TerminalKey { made_at: None, ..said });
        let own = api::Own::from(OwnPane::new(self::key(3), 12).unwrap());
        let both = UnreadFile { own: Some(own), ..in_terminal(said) };
        for unplaceable in [zero, unmade, both] {
            let files = std::slice::from_ref(&unplaceable);
            assert_eq!(unread(1, files), Err(unplaced(1, 1)), "{unplaceable:?}");
        }
    }
}
