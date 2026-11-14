use std::time::{Duration, SystemTime};

use std::path::Path;

use lunetad::mux::own::OwnKey;
use lunetad::{log, Destination, Directory};

use crate::away::ANSWER;
use crate::ledger::Session;
use crate::machines;
use crate::news::News;
use crate::own_sessions;
use crate::sidebar::{NoRefuge, Sidebar};
use crate::zellij::{self, Refuge, SessionName};

pub enum First {
    Here(SessionName),
    Away(Destination, Refuge),
    Make(Option<SessionName>),
    Own(OwnKey),
}

pub enum Start {
    Now(First),
    Hear { machines: Vec<Destination>, named: Option<SessionName> },
}

struct Running {
    name: SessionName,
    started: SystemTime,
    seen: Option<SystemTime>,
}

enum Here {
    Join(SessionName),
    Nothing { holding: Vec<SessionName> },
}

pub fn named(session: SessionName) -> Result<Start, String> {
    make(Some(session)).map(Start::Now)
}

pub fn own(named: &SessionName, dir: &Path) -> Result<Start, String> {
    let running = zellij::running()?;
    if running.iter().any(|(name, _)| name == named) {
        return Err(format!("{named} is a session of zellij's: name another for an own one"));
    }
    let session = if let Some(session) = own_sessions::named(named)? {
        session
    } else {
        let unread = || format!("{} is no directory an own session starts in", dir.display());
        let cwd = dir.to_str().and_then(|dir| Directory::parse(dir.to_owned()).ok());
        let cwd = cwd.ok_or_else(unread)?;
        own_sessions::made(named, &cwd).map_err(|why| format!("no own session made: {why}"))?
    };
    if let Some(refusal) = own_sessions::refusal(session, named) {
        return Err(refusal);
    }
    Ok(Start::Now(First::Own(session)))
}

pub fn bare(named: Option<SessionName>, news: &News) -> Result<Start, String> {
    let holding = match here(named.as_ref(), news) {
        None => return make(named).map(Start::Now),
        Some(Here::Join(session)) => return Ok(Start::Now(First::Here(session))),
        Some(Here::Nothing { holding }) => holding,
    };
    let machines = machines::named().unwrap_or_else(|why| {
        eprintln!("luneta: looking on no other machine: {why}");
        Vec::new()
    });
    if !machines.is_empty() {
        return Ok(Start::Hear { machines, named });
    }
    left(named, &holding, &[]).map(Start::Now)
}

pub fn heard(named: Option<SessionName>, sidebar: &Sidebar) -> Result<First, String> {
    let holding = match here(named.as_ref(), sidebar.news()) {
        None => return make(named),
        Some(Here::Join(session)) => return Ok(First::Here(session)),
        Some(Here::Nothing { holding }) => holding,
    };
    match away(sidebar, &[], ANSWER) {
        Sought::Found(destination, refuge) => Ok(First::Away(destination, refuge)),
        Sought::Nowhere { held } => left(named, &holding, &held),
    }
}

pub enum Sought {
    Found(Destination, Refuge),
    Nowhere { held: Vec<Destination> },
}

pub fn away(sidebar: &Sidebar, tried: &[Destination], limit: Duration) -> Sought {
    let mut held = Vec::new();
    for (destination, refuges) in sidebar.refuges_away(tried, zellij::within()) {
        let sessions = match refuges {
            Ok(sessions) => sessions,
            Err(why) => {
                log::say(&format!("{destination}: no session to go on in: {why}"));
                if why == NoRefuge::Held {
                    held.push(destination);
                }
                continue;
            },
        };
        match zellij::answering_away(&destination, sessions, limit) {
            Ok(Some(refuge)) => return Sought::Found(destination, refuge),
            Ok(None) => log::say(&format!("{destination}: no session to go on in answers")),
            Err(why) => {
                log::say(&format!("no session of another machine's to go on in: {why}"));
                break;
            },
        }
    }
    Sought::Nowhere { held }
}

fn here(named: Option<&SessionName>, news: &News) -> Option<Here> {
    let running = zellij::settled()?.into_iter();
    let (holding, free): (Vec<_>, Vec<_>) =
        running.partition(|(name, _)| zellij::may_hold_terminal(name));
    let free = free.into_iter().map(|(name, id)| Running {
        name,
        started: id.started(),
        seen: Session::of(id).and_then(|session| news.last_seen(session)),
    });
    Some(match zellij::answering(joinable(free.collect(), named)) {
        Some(Refuge { session, .. }) => Here::Join(session),
        None => Here::Nothing { holding: holding.into_iter().map(|(name, _)| name).collect() },
    })
}

fn joinable(mut running: Vec<Running>, named: Option<&SessionName>) -> Vec<SessionName> {
    let is_named = |session: &Running| named == Some(&session.name);
    running.sort_by(|a, b| {
        (is_named(b), b.seen, b.started, &b.name).cmp(&(is_named(a), a.seen, a.started, &a.name))
    });
    running.into_iter().map(|session| session.name).collect()
}

fn left(
    named: Option<SessionName>,
    holding: &[SessionName],
    held: &[Destination],
) -> Result<First, String> {
    match refusal_to_make(holding, held) {
        Some(refusal) => Err(refusal),
        None => make(named),
    }
}

fn make(named: Option<SessionName>) -> Result<First, String> {
    match named.as_ref().and_then(zellij::refusal) {
        Some(refusal) => Err(refusal),
        None => Ok(First::Make(named)),
    }
}

fn refusal_to_make(holding: &[SessionName], held: &[Destination]) -> Option<String> {
    let mut holders = Vec::new();
    if !holding.is_empty() {
        let sessions: Vec<String> = holding.iter().map(|name| format!("\"{name}\"")).collect();
        holders.push(format!("this terminal is a pane of {}", sessions.join(" or ")));
    }
    if !held.is_empty() {
        let machines: Vec<String> = held.iter().map(ToString::to_string).collect();
        let machines = machines.join(", ");
        holders.push(format!("each session of {machines} may hold this terminal in a pane"));
    }
    (!holders.is_empty()).then(|| {
        let holders = holders.join(", and ");
        format!(
            "{holders}, which zellij would show inside itself: name another session, or run \
             luneta outside zellij"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn running(name: &str, started: u64, seen: Option<u64>) -> Running {
        Running { name: self::name(name), started: at(started), seen: seen.map(at) }
    }

    fn name(name: &str) -> SessionName {
        SessionName::running(name).unwrap()
    }

    fn order(sessions: Vec<Running>, named: Option<&str>) -> Vec<String> {
        let named = named.map(name);
        let joinable = joinable(sessions, named.as_ref());
        joinable.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_bare_luneta_tries_its_directory_s_then_the_last_seen_then_the_newest() {
        let sessions = || {
            vec![
                running("newest", 40, None),
                running("older", 30, None),
                running("seen-long-ago", 10, Some(100)),
                running("seen-last", 20, Some(200)),
                running("project", 5, None),
            ]
        };
        let by_news = ["seen-last", "seen-long-ago", "newest", "older", "project"];
        assert_eq!(order(sessions(), None), by_news);
        assert_eq!(order(sessions(), Some("no-such-session")), by_news);
        let in_project = ["project", "seen-last", "seen-long-ago", "newest", "older"];
        assert_eq!(order(sessions(), Some("project")), in_project);
    }

    #[test]
    fn of_two_alike_the_one_whose_name_comes_last_goes_first() {
        let sessions = vec![running("a", 2, None), running("b", 2, None)];
        assert_eq!(order(sessions, None), ["b", "a"]);
    }

    #[test]
    fn luneta_refuses_to_make_a_session_only_where_it_passed_sessions_over() {
        let work = [name("work")];
        assert_eq!(refusal_to_make(&[], &[]), None);
        assert_eq!(
            refusal_to_make(&work, &[]).unwrap(),
            "this terminal is a pane of \"work\", which zellij would show inside itself: name \
             another session, or run luneta outside zellij"
        );
        let held: [Destination; 2] = ["pi".parse().unwrap(), "me@vps".parse().unwrap()];
        assert_eq!(
            refusal_to_make(&[], &held).unwrap(),
            "each session of pi, me@vps may hold this terminal in a pane, which zellij would show \
             inside itself: name another session, or run luneta outside zellij"
        );
        let two = [name("work"), name("play")];
        let two = refusal_to_make(&two, &[]).unwrap();
        assert!(two.starts_with("this terminal is a pane of \"work\" or \"play\", which"), "{two}");
        let both = refusal_to_make(&work, &held[..1]).unwrap();
        assert!(
            both.starts_with(
                "this terminal is a pane of \"work\", and each session of pi may hold this \
                 terminal in a pane, which"
            ),
            "{both}"
        );
    }
}
