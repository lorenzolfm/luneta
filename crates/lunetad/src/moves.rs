//! What zellij's CLI does with a client on the machine it runs on: takes the
//! one that last typed in a session to another session, or to a pane, and
//! finds the session one goes on in once its own ended. luneta has zellij do
//! so for its own client on its machine; a daemon has it do so here for a
//! luneta on another machine, whose client runs here through ssh.

use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::api;
use crate::program;
use crate::zellij;

/// How long zellij has to take an action: a server that runs answers at once.
const LIMIT: Duration = Duration::from_secs(2);

/// What zellij says as it fails to focus a pane that has the focus already,
/// which is where the client was to go, and no failure.
pub const FOCUSED: &str = "is already focused";

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

/// `zellij`, a command of zellij's set up as its caller needs, with the
/// action that takes the client that last typed in the session `at` to the
/// pane `pane` of `to`, or to `to` alone, on the pane it last had the focus
/// on. None where the client is there already: in `at`, with no pane to go
/// to.
#[must_use]
pub fn go(mut zellij: Command, at: &str, to: &str, pane: Option<u32>) -> Option<Command> {
    let pane = pane.map(|id| format!("terminal_{id}"));
    match pane {
        Some(pane) if at == to => {
            zellij.args(["--session", at, "action", "focus-pane-id", &pane]);
        },
        None if at == to => return None,
        pane => {
            zellij.args(["--session", at, "action", "switch-session", to]);
            zellij.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
        },
    }
    Some(zellij)
}

/// Has zellij take the action `go` makes, or says why it did not, as zellij
/// said it.
pub fn went(at: &str, to: &str, pane: Option<u32>) -> Result<(), String> {
    let Some(mut zellij) = go(Command::new("zellij"), at, to, pane) else {
        return Ok(());
    };
    let ran = program::run(&mut zellij, LIMIT)
        .map_err(|error| format!("zellij could not be run: {error}"))?;
    match ran.answer() {
        Ok(_) => Ok(()),
        Err(said) if said.contains(FOCUSED) => Ok(()),
        Err(said) => Err(format!("zellij: {said}")),
    }
}

/// A session a client can go on in: one that runs, as it answered for its
/// clients, which are those it has before the client comes.
pub struct Refuge {
    pub session: String,
    pub before: Vec<api::Client>,
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
/// run that `clients` answers for, which the picker lists first. Never
/// `current`.
///
/// A socket can outlive its server, as one that crashed, and a session can
/// end as it is looked for. `attach` brings back a session that ended, where
/// zellij kept it to resurrect, and otherwise zellij's client fails: only a
/// session that answers is one to go on in.
pub fn refuge(
    folder: &Path,
    seats: &[&str],
    current: Option<&str>,
    mut clients: impl FnMut(&str) -> Option<Vec<api::Client>>,
) -> Ended {
    let deadline = Instant::now() + ENDING;
    loop {
        let Some(running) = zellij::running(folder) else {
            return Ended::Kept;
        };
        if gone(seats, &running) {
            return Ended::Went(sought(running, current, &mut clients));
        }
        if Instant::now() >= deadline {
            return Ended::Kept;
        }
        thread::sleep(ENDING_LOOK);
    }
}

/// The newest session that runs, as zellij's sockets in `folder` say, that
/// `clients` answers for, where a client goes on that ended elsewhere: never
/// `current`. None where none answers, and where not all of `folder` reads.
pub fn newest(
    folder: &Path,
    current: Option<&str>,
    mut clients: impl FnMut(&str) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    sought(zellij::running(folder)?, current, &mut clients)
}

/// The first of `running` that `clients` answers for, as `refuges` orders
/// them, within `SEEKING`.
fn sought(
    running: Vec<(String, SystemTime)>,
    current: Option<&str>,
    clients: &mut impl FnMut(&str) -> Option<Vec<api::Client>>,
) -> Option<Refuge> {
    let seeking = Instant::now() + SEEKING;
    let sessions = refuges(running, current).into_iter();
    sessions.take_while(|_| Instant::now() < seeking).find_map(|session| {
        let before = clients(&session)?;
        Some(Refuge { session, before })
    })
}

/// Whether the session of a client that ended in one of `seats` went with
/// it: whether any of them is not among `running`. A client on its way from
/// one session to another may have ended in either, and one of the two gone
/// is taken for the client's: one that detached as it went to a session
/// that never came up goes on, rather than one whose session was killed as
/// it left ending luneta.
fn gone(seats: &[&str], running: &[(String, SystemTime)]) -> bool {
    seats.iter().any(|seat| running.iter().all(|(name, _)| name != seat))
}

/// The names of `running` that a client can go on in, newest first, by when
/// each started and then by name, as the picker lists them: all but
/// `current`.
fn refuges(mut running: Vec<(String, SystemTime)>, current: Option<&str>) -> Vec<String> {
    running.retain(|(name, _)| current != Some(name.as_str()));
    running.sort_by(|(a, a_at), (b, b_at)| (b_at, b).cmp(&(a_at, a)));
    running.into_iter().map(|(name, _)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refuge is sought in the order the picker lists sessions: the
    /// newest first, and of two that started at once, the one whose name
    /// comes last, as the sidebar has them. Never the current one, which
    /// zellij would show inside itself.
    #[test]
    fn the_refuge_is_sought_in_the_order_the_picker_lists_sessions() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        let running = [("b", at(2)), ("c", at(1)), ("a", at(2))];
        let running = running.map(|(name, started)| (name.to_owned(), started)).to_vec();
        assert_eq!(refuges(running.clone(), None), ["b", "a", "c"]);
        assert_eq!(refuges(running, Some("b")), ["a", "c"]);
        assert!(refuges(Vec::new(), None).is_empty());
    }

    /// A client that ended on its way from one session to another may have
    /// been in either: its session went where either is gone, and the client
    /// is taken for one that detached only where both run.
    #[test]
    fn a_client_s_session_went_where_any_it_may_have_been_in_did() {
        let running = |names: &[&str]| -> Vec<(String, SystemTime)> {
            names.iter().map(|name| ((*name).to_owned(), SystemTime::UNIX_EPOCH)).collect()
        };
        assert!(!gone(&["a", "b"], &running(&["a", "b", "c"])));
        assert!(gone(&["a", "b"], &running(&["b", "c"])));
        assert!(gone(&["a", "b"], &running(&["a", "c"])));
        assert!(gone(&["a"], &running(&[])));
    }
}
