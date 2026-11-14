use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::api;
use crate::mux::{Client, Mux, SessionName, SocketFolder, Zellij};

const ENDING: Duration = Duration::from_millis(100);

const ENDING_LOOK: Duration = Duration::from_millis(10);

const SEEKING: Duration = Duration::from_secs(1);

pub struct Refuge {
    pub session: SessionName,
    pub before: Vec<api::Client>,
    pub started: Option<SystemTime>,
}

pub enum Ended {
    Kept,
    Went(Option<Refuge>),
}

pub fn refuge(
    folder: &SocketFolder,
    seats: &[SessionName],
    mut clients: impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Ended {
    let deadline = Instant::now() + ENDING;
    loop {
        let Some(running) = folder.running() else {
            return Ended::Kept;
        };
        if gone(seats, &running) {
            return Ended::Went(sought(running, &mut clients));
        }
        if Instant::now() >= deadline {
            return Ended::Kept;
        }
        thread::sleep(ENDING_LOOK);
    }
}

pub fn newest(
    folder: &SocketFolder,
    mut clients: impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    sought(folder.running()?, &mut clients)
}

fn sought(
    running: Vec<(SessionName, SystemTime)>,
    clients: &mut impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    let running = refuges(running).into_iter().map(|(session, started)| (session, Some(started)));
    answering_since(running, clients)
}

#[must_use]
pub fn before(session: &SessionName) -> Option<Vec<api::Client>> {
    let rows = Mux::Zellij(session).client_rows().ok()?;
    Some(rows.into_iter().map(Client::to_api).collect())
}

pub fn answering(
    sessions: impl IntoIterator<Item = SessionName>,
    mut clients: impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    answering_since(sessions.into_iter().map(|session| (session, None)), &mut clients)
}

fn answering_since(
    sessions: impl IntoIterator<Item = (SessionName, Option<SystemTime>)>,
    clients: &mut impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    let seeking = Instant::now() + SEEKING;
    let sessions = sessions.into_iter().filter(|(session, _)| Zellij::attaches_to(session));
    let mut sessions = sessions.take_while(|_| Instant::now() < seeking);
    sessions.find_map(|(session, started)| {
        let before = clients(&session)?;
        Some(Refuge { session, before, started })
    })
}

fn gone(seats: &[SessionName], running: &[(SessionName, SystemTime)]) -> bool {
    seats.iter().any(|seat| running.iter().all(|(name, _)| name != seat))
}

fn refuges(mut running: Vec<(SessionName, SystemTime)>) -> Vec<(SessionName, SystemTime)> {
    running.sort_by(|(a, a_at), (b, b_at)| (b_at, b).cmp(&(a_at, a)));
    running
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refuge_is_sought_in_the_order_the_picker_lists_sessions() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        let name = |name: &str| SessionName::running(name).unwrap();
        let running = [("b", at(2)), ("c", at(1)), ("a", at(2))];
        let running = running.map(|(it, started)| (name(it), started)).to_vec();
        let sought = [("b", at(2)), ("a", at(2)), ("c", at(1))];
        assert_eq!(refuges(running), sought.map(|(it, started)| (name(it), started)));
        assert_eq!(refuges(Vec::new()), []);
    }

    #[test]
    fn the_refuge_is_the_first_session_that_answers() {
        let sessions = ["gone", "up", "later"].map(|name| SessionName::running(name).unwrap());
        let mut asked = Vec::new();
        let found = answering(sessions, |session| {
            asked.push(session.to_string());
            (session.as_str() != "gone").then(Vec::new)
        });
        assert_eq!(found.map(|refuge| refuge.session.to_string()), Some("up".to_owned()));
        assert_eq!(asked, ["gone", "up"]);
    }

    #[test]
    fn the_refuge_is_never_one_zellij_s_client_does_not_attach_to() {
        let sessions = ["-c", "up"].map(|name| SessionName::running(name).unwrap());
        let mut asked = Vec::new();
        let found = answering(sessions, |session| {
            asked.push(session.to_string());
            Some(Vec::new())
        });
        assert_eq!(found.map(|refuge| refuge.session.to_string()), Some("up".to_owned()));
        assert_eq!(asked, ["up"]);
    }

    #[test]
    fn a_client_s_session_went_where_any_it_may_have_been_in_did() {
        let running = |names: &[&str]| -> Vec<(SessionName, SystemTime)> {
            let at = |name: &&str| (SessionName::running(name).unwrap(), SystemTime::UNIX_EPOCH);
            names.iter().map(at).collect()
        };
        let seats = |names: &[&str]| -> Vec<SessionName> {
            names.iter().map(|name| SessionName::running(name).unwrap()).collect()
        };
        assert!(!gone(&seats(&["a", "b"]), &running(&["a", "b", "c"])));
        assert!(gone(&seats(&["a", "b"]), &running(&["b", "c"])));
        assert!(gone(&seats(&["a", "b"]), &running(&["a", "c"])));
        assert!(gone(&seats(&["a"]), &running(&[])));
    }
}
