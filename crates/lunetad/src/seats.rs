//! Which pane of which session each agent sits in, as the daemon's host has
//! them: the sidebar's rows show each agent in its pane, and a move to another
//! session starts each anew by what it is.

use std::collections::HashMap;

use crate::api::session::Mux;
use crate::api::{agent, Agent, Harness, Pane, Session, Zellij};

/// The agent in each pane, by its session and the pane's id. A pane holds one:
/// the agent started at a prompt, or else the one that started first. An
/// agent is in the session the daemon follows its pane to, or, where the
/// daemon cannot tell, the one `found` finds; or in an own pane, in the own
/// session of its id.
#[must_use]
pub fn seated<'a>(
    agents: &'a [Agent],
    sessions: &'a [Session],
) -> HashMap<(&'a str, u32), &'a Agent> {
    let mut seated: HashMap<(&str, u32), &Agent> = HashMap::new();
    for agent in agents {
        let seat = match &agent.pane {
            Some(agent::Pane::Zellij(zellij)) => zellij
                .session
                .as_deref()
                .or_else(|| found(agent, zellij, sessions))
                .map(|session| (session, zellij.pane)),
            Some(agent::Pane::Own(own)) => owned(own.session, sessions).map(|at| (at, own.pane)),
            None => None,
        };
        let Some((session, pane)) = seat else {
            continue;
        };
        let rank = |agent: &Agent| {
            let started = agent.started_at.as_ref().map(|at| (at.seconds, at.nanos));
            (!agent.at_a_prompt(), started)
        };
        seated
            .entry((session, pane))
            .and_modify(|seat| {
                if rank(agent) < rank(seat) {
                    *seat = agent;
                }
            })
            .or_insert(agent);
    }
    seated
}

/// The name of the own session of id `id`, where one runs.
fn owned(id: u64, sessions: &[Session]) -> Option<&str> {
    let own = |session: &&Session| matches!(&session.mux, Some(Mux::Own(own)) if own.id == id);
    sessions.iter().find(own).map(|session| session.name.as_str())
}

/// The session of an agent the daemon cannot follow to one, as it cannot one
/// renamed before it started, found by where it works: the one session that
/// lists a pane of the agent's id that works where the agent does. Of more
/// than one, the session the agent's environment names, or else, for Claude
/// Code's, the one whose pane runs `claude` or an agent titled, if one alone
/// does: what a pane runs tells of no other harness's agent.
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
    let holders: Vec<(&str, &Pane)> = sessions.iter().filter_map(pane).collect();
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
