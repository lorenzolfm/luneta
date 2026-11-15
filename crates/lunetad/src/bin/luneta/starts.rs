use std::time::SystemTime;

use std::path::Path;

use lunetad::mux::own::{server, OwnKey};
use lunetad::names::session_for;

use crate::ledger::Session;
use crate::news::News;
use crate::own_sessions::{self, Unmade};
use crate::zellij::{self, SessionName};

struct Running {
    key: OwnKey,
    name: SessionName,
    seen: Option<SystemTime>,
}

pub fn own(named: &SessionName, dir: &Path) -> Result<OwnKey, String> {
    let running = zellij::running()?;
    if running.iter().any(|(name, _)| name == named) {
        return Err(format!("{named} is a session of zellij's: name another for an own one"));
    }
    let session = if let Some(session) = own_sessions::named(named)? {
        session
    } else {
        let cwd = own_sessions::directory(dir)?;
        match own_sessions::made(named, &cwd) {
            Ok(session) => session,
            Err(Unmade::Taken(_)) => own_sessions::named(named)?
                .ok_or_else(|| format!("another luneta made {named}, which ended already"))?,
            Err(Unmade::Failed(why)) => return Err(format!("no own session made: {why}")),
        }
    };
    if let Some(refusal) = own_sessions::refusal(session, named) {
        return Err(refusal);
    }
    Ok(session)
}

pub fn bare(dir: &Path, news: &News) -> Result<OwnKey, String> {
    let named = session_for(dir);
    let sessions = server::sessions().map_err(|failure| failure.to_string())?;
    let held = own_sessions::holding().map_err(|why| {
        format!("{why}, so luneta may run in any own session, and joins none: unset them")
    })?;
    let free = sessions.iter().filter(|listing| Some(listing.key()) != held);
    let free: Vec<Running> = free
        .map(|listing| Running {
            key: listing.key(),
            name: listing.name.clone(),
            seen: news.last_seen(Session::own(listing.made, listing.id)),
        })
        .collect();
    if let Some(joined) = to_join(&free, named.as_ref()) {
        return Ok(joined);
    }
    match named {
        Some(named) => own(&named, dir),
        None => Err(format!("{} names no own session: name one", dir.display())),
    }
}

fn to_join(running: &[Running], named: Option<&SessionName>) -> Option<OwnKey> {
    let rank = |session: &Running| (named == Some(&session.name), session.seen, session.key.made);
    let ranked = |a: &&Running, b: &&Running| (rank(a), &a.name).cmp(&(rank(b), &b.name));
    running.iter().max_by(ranked).map(|session| session.key)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lunetad::mux::own::{OwnId, OwnMade};

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn running(id: u64, made: u64, seen: Option<u64>) -> Running {
        let made = OwnMade::from_nanos(made * 1_000_000_000);
        let key = OwnKey { id: OwnId::new(id).unwrap(), made };
        Running { key, name: name(&format!("s{id}")), seen: seen.map(at) }
    }

    fn name(name: &str) -> SessionName {
        SessionName::running(name).unwrap()
    }

    fn order(mut sessions: Vec<Running>, named: Option<&str>) -> Vec<u64> {
        let named = named.map(name);
        let mut order = Vec::with_capacity(sessions.len());
        for _ in 0..sessions.len() {
            let joined = to_join(&sessions, named.as_ref()).unwrap();
            sessions.retain(|session| session.key != joined);
            order.push(joined.id.get());
        }
        assert_eq!(to_join(&sessions, named.as_ref()), None);
        order
    }

    #[test]
    fn a_bare_luneta_tries_its_directory_s_then_the_last_seen_then_the_newest() {
        let sessions = || {
            vec![
                running(1, 40, None),
                running(2, 30, None),
                running(3, 10, Some(100)),
                running(4, 20, Some(200)),
                running(5, 5, None),
            ]
        };
        let by_news = [4, 3, 1, 2, 5];
        assert_eq!(order(sessions(), None), by_news);
        assert_eq!(order(sessions(), Some("no-such-session")), by_news);
        assert_eq!(order(sessions(), Some("s5")), [5, 4, 3, 1, 2]);
    }

    #[test]
    fn of_two_alike_the_one_whose_name_comes_last_goes_first() {
        let sessions = vec![running(1, 2, None), running(2, 2, None)];
        assert_eq!(order(sessions, None), [2, 1]);
    }
}
