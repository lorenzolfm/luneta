//! The session a client on the machine zellij runs on goes on in once its
//! own ended. luneta finds it for its own client on its machine; a daemon
//! finds it here for a luneta on another machine, whose client runs here
//! through ssh.

use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::api;
use crate::mux::{Client, Mux, SessionName, SocketFolder, Zellij};

/// How long the refuge is looked for the session its client was in to be
/// gone, once the client ended. zellij takes the session's socket away a
/// moment before its clients end, measured at under a millisecond, and a
/// client that detached leaves it there: that one costs this long.
const ENDING: Duration = Duration::from_millis(100);

/// How often it looks meanwhile.
const ENDING_LOOK: Duration = Duration::from_millis(10);

/// How long one session after another is tried for one that answers, once
/// the client's is gone. A session whose server is gone answers at once that
/// it does not run, and a server that hangs holds up every zellij command for
/// as long as its clients are waited for, which is longer: it costs one try.
const SEEKING: Duration = Duration::from_secs(1);

/// A session a client can go on in: one that runs, as it answered for its
/// clients, which are those it has before the client comes.
pub struct Refuge {
    pub session: SessionName,
    pub before: Vec<api::Client>,
    /// When it started, as its socket said where it was sought among this
    /// machine's: none where it was not, as of another machine's.
    pub started: Option<SystemTime>,
}

/// What became of the session of a client that ended.
pub enum Ended {
    /// It runs still, as the client detached; or not all of zellij's folder
    /// reads, which could hold it.
    Kept,
    /// It went with the client, as a session does once zellij quits, its last
    /// pane exits, or it is killed: the client goes on in this one, where any
    /// answers.
    Went(Option<Refuge>),
}

/// What became of the session of a client that ended in one of `seats`, the
/// sessions it may have been in, as zellij's sockets in `folder` say, and
/// where the client goes on where it went: the newest session of those that
/// run that `clients` answers for, which the picker lists first. Never one
/// of a name luneta cannot hand zellij, and never one `clients` does not
/// answer for, as one that may hold the terminal of the client's luneta.
///
/// A socket can outlive its server, as one that crashed, and a session can
/// end as it is looked for. `attach` brings back a session that ended, where
/// zellij kept it to resurrect, and otherwise zellij's client fails: only a
/// session that answers is one to go on in.
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

/// The newest session that runs, as zellij's sockets in `folder` say, that
/// `clients` answers for, where a client goes on that ended elsewhere: never
/// one of a name luneta cannot hand zellij. None where none answers, and
/// where not all of `folder` reads.
pub fn newest(
    folder: &SocketFolder,
    mut clients: impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    sought(folder.running()?, &mut clients)
}

/// The first of `running` of a name luneta can hand zellij that `clients`
/// answers for, as `refuges` orders them, within `SEEKING`.
fn sought(
    running: Vec<(SessionName, SystemTime)>,
    clients: &mut impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    let running = refuges(running).into_iter().map(|(session, started)| (session, Some(started)));
    answering_since(running, clients)
}

/// What `refuge`'s `clients` answers for `session` on this machine: every row
/// zellij lists of its clients, as `Mux::client_rows` says them, or none
/// where zellij did not list them. A client that comes to it is none of these,
/// and zellij is asked for no panes, which a session that answers may not
/// list.
#[must_use]
pub fn before(session: &SessionName) -> Option<Vec<api::Client>> {
    let rows = Mux::Zellij(session).client_rows().ok()?;
    Some(rows.into_iter().map(Client::to_api).collect())
}

/// The first of `sessions` that `clients` answers for, tried one after
/// another within `SEEKING`, where a client goes on: of this machine's, or
/// of another's, as its daemon there lists their clients. Never one zellij's
/// client does not attach to, as `Zellij::attaches_to` says, which is not asked.
pub fn answering(
    sessions: impl IntoIterator<Item = SessionName>,
    mut clients: impl FnMut(&SessionName) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    answering_since(sessions.into_iter().map(|session| (session, None)), &mut clients)
}

/// The first of `sessions` that `clients` answers for, as `answering` says,
/// each with when it started, if known.
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

/// Whether the session of a client that ended in one of `seats` went with
/// it: whether any of them is not among `running`. A client on its way from
/// one session to another may have ended in either, and one of the two gone
/// is taken for the client's: one that detached as it went to a session
/// that never came up goes on, rather than one whose session was killed as
/// it left ending luneta.
fn gone(seats: &[SessionName], running: &[(SessionName, SystemTime)]) -> bool {
    seats.iter().any(|seat| running.iter().all(|(name, _)| name != seat))
}

/// `running`, the sessions a client can go on in, newest first, by when
/// each started and then by name, as the picker lists them.
fn refuges(mut running: Vec<(SessionName, SystemTime)>) -> Vec<(SessionName, SystemTime)> {
    running.sort_by(|(a, a_at), (b, b_at)| (b_at, b).cmp(&(a_at, a)));
    running
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refuge is sought in the order the picker lists sessions: the
    /// newest first, and of two that started at once, the one whose name
    /// comes last, as the sidebar has them.
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

    /// The refuge is the first session that answers for its clients, tried
    /// in turn: one whose server is gone, as one whose socket outlived it,
    /// is passed over, and none is asked after the one that answers.
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

    /// A session zellij's client does not attach to is passed over unasked,
    /// though it answers: a client sent there would fail.
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

    /// A client that ended on its way from one session to another may have
    /// been in either: its session went where either is gone, and the client
    /// is taken for one that detached only where both run.
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
