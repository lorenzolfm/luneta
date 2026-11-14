use std::collections::HashMap;
use std::num::NonZeroU32;

use luneta_protocol::unread::Unplaced;

use crate::api::session::Mux;
use crate::api::{agent, Agent, Harness, Pane, Session, UnreadFile, Zellij};
use crate::mux::{OwnKey, OwnPane};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Where<'a> {
    Zellij(&'a str),
    Own(OwnKey),
}

#[must_use]
pub fn seated<'a>(
    agents: &'a [Agent],
    sessions: &'a [Session],
) -> HashMap<(Where<'a>, u32), &'a Agent> {
    let mut seated: HashMap<(Where, u32), &Agent> = HashMap::new();
    for agent in agents {
        let seat = match &agent.pane {
            Some(agent::Pane::Zellij(zellij)) => zellij
                .session
                .as_deref()
                .or_else(|| found(agent, zellij, sessions))
                .map(|session| (Where::Zellij(session), zellij.pane)),
            Some(agent::Pane::Own(own)) => {
                OwnPane::of(own).ok().map(|own| (Where::Own(own.session), own.id.get()))
            },
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

fn found<'a>(agent: &Agent, zellij: &Zellij, sessions: &'a [Session]) -> Option<&'a str> {
    if agent.cwd.is_empty() {
        return None;
    }
    let pane = |session: &'a Session| {
        let mut panes = session.panes.iter().flat_map(|panes| &panes.panes);
        let pane = panes.find(|pane| {
            pane.id == zellij.pane && pane.cwd.as_deref() == Some(agent.cwd.as_str())
        })?;
        Some((session.name.as_str(), pane))
    };
    let of_zellij = |session: &&Session| matches!(session.multiplexer(), Mux::Zellij(_));
    let holders: Vec<(&str, &Pane)> = sessions.iter().filter(of_zellij).filter_map(pane).collect();
    if let [(only, _)] = holders.as_slice() {
        return Some(only);
    }
    if let Some((named, _)) = holders.iter().find(|(name, _)| *name == zellij.named) {
        return Some(named);
    }
    match agent.runs_in() {
        Harness::ClaudeCode => {},
        Harness::Pi | Harness::Unspecified => return None,
    }
    let claude = |pane: &Pane| {
        pane.command.as_deref().is_some_and(|run| run.contains("claude")) || pane.titled.is_some()
    };
    let mut claudes = holders.iter().filter(|(_, pane)| claude(pane));
    match (claudes.next(), claudes.next()) {
        (Some((one, _)), None) => Some(one),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadSeat<'a> {
    NoPane,
    Zellij { session: &'a str, pane: u32 },
    Own(OwnPane),
    Unplaced,
}

impl<'a> UnreadSeat<'a> {
    #[must_use]
    pub fn of(file: &'a UnreadFile) -> Self {
        match (&file.zellij, &file.own) {
            (None, None) => UnreadSeat::NoPane,
            (Some(zellij), None) => match zellij.session.as_deref() {
                Some(session) => UnreadSeat::Zellij { session, pane: zellij.pane },
                None => UnreadSeat::Unplaced,
            },
            (None, Some(own)) => OwnPane::of(own).map_or(UnreadSeat::Unplaced, UnreadSeat::Own),
            (Some(_), Some(_)) => UnreadSeat::Unplaced,
        }
    }
}

pub fn unread(told: u32, files: &[UnreadFile]) -> Result<Vec<(Where<'_>, u32)>, Unplaced> {
    let listed = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let unread_count = told.max(listed);
    let mut unplaced = unread_count - listed;
    let mut seats = Vec::with_capacity(files.len());
    for file in files {
        match UnreadSeat::of(file) {
            UnreadSeat::NoPane => {},
            UnreadSeat::Zellij { session, pane } => seats.push((Where::Zellij(session), pane)),
            UnreadSeat::Own(own) => seats.push((Where::Own(own.session), own.id.get())),
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

    fn file(pid: u32, zellij: Option<(&str, Option<&str>, u32)>) -> UnreadFile {
        let zellij = zellij.map(|(named, session, pane)| Zellij {
            named: named.to_owned(),
            session: session.map(str::to_owned),
            pane,
        });
        UnreadFile { pid, zellij, own: None }
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
        let placed = [file(10, Some(("work", Some("play"), 3))), file(11, None)];
        assert_eq!(unread(2, &placed), Ok(vec![(Where::Zellij("play"), 3)]));
        assert_eq!(unread(0, &[]), Ok(Vec::new()));

        let lost = [file(10, Some(("work", Some("work"), 3))), file(12, Some(("work", None, 4)))];
        assert_eq!(unread(2, &lost), Err(unplaced(2, 1)));
        assert_eq!(unread(3, &[]), Err(unplaced(3, 3)));
        assert_eq!(unread(3, &lost), Err(unplaced(3, 2)));
        assert_eq!(unread(1, &lost), Err(unplaced(2, 1)));
    }

    #[test]
    fn a_file_in_an_own_pane_is_in_its_own_session() {
        let in_own = |own: api::Own| UnreadFile { own: Some(own), ..file(10, None) };
        let pane = api::Own::from(OwnPane::new(key(3), 12).unwrap());
        assert_eq!(unread(1, &[in_own(pane)]), Ok(vec![(Where::Own(key(3)), 12)]));

        let zero = in_own(api::Own { pane: 0, ..pane });
        let unkeyed = in_own(api::Own { session: None, ..pane });
        let both = UnreadFile { own: Some(pane), ..file(10, Some(("work", Some("work"), 3))) };
        for unplaceable in [zero, unkeyed, both] {
            let files = std::slice::from_ref(&unplaceable);
            assert_eq!(unread(1, files), Err(unplaced(1, 1)), "{unplaceable:?}");
        }
    }
}
