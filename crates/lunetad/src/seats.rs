use std::collections::HashMap;

use crate::api::session::Mux;
use crate::api::{agent, Agent, Harness, Pane, Session, Zellij};
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
