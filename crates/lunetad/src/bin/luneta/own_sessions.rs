//! The own sessions of this machine, as luneta goes to them: made by the
//! daemon, which alone starts their server, and never the one luneta runs
//! in, whose client would show luneta in itself, and each key typed in it go
//! round again. Each goes as no other is, by its id and when its server made
//! it, as the next server counts its ids from 1 again.

use std::time::Duration;

use lunetad::api::{NewOwnRequest, OwnSession};
use lunetad::mux::own::{self, server, OwnId, OwnKey};
use lunetad::{log, Directory, SessionName};

use crate::daemon;

/// How long luneta waits for the daemon to make an own session: for it to
/// start, where none runs, and for the server to, as `server::made` waits.
const MAKING: Duration = Duration::from_secs(20);

/// The own session luneta runs in, where it runs in a pane of one.
fn holding() -> Option<OwnKey> {
    own::here().map(|pane| pane.session)
}

/// Why luneta would not go to the own session `session`: it runs in a pane
/// of it.
pub fn refusal(session: OwnKey, name: &SessionName) -> Option<String> {
    (holding() == Some(session))
        .then(|| format!("luneta runs in {name}, and goes nowhere it shows itself"))
}

/// The own session named `name`, where one runs by it.
pub fn named(name: &SessionName) -> Result<Option<OwnKey>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.key()))
}

/// The newest own session that runs, by when its server made it, but the
/// one luneta runs in and those `passed`: none where none runs, or the server
/// does not answer, which the log says.
pub fn newest(passed: &[OwnKey]) -> Option<OwnKey> {
    let sessions = server::sessions()
        .inspect_err(|failure| log::say(&format!("no own session to go on in: {failure}")))
        .ok()?;
    newest_of(sessions.iter().map(server::Listing::key), holding(), passed)
}

/// The newest of `sessions`, by when their server made each, and then by
/// id, but `held` and those `passed`.
fn newest_of(
    sessions: impl Iterator<Item = OwnKey>,
    held: Option<OwnKey>,
    passed: &[OwnKey],
) -> Option<OwnKey> {
    let free = sessions.filter(|key| Some(*key) != held && !passed.contains(key));
    free.max_by_key(|key| (key.made, key.id))
}

/// Has the daemon make an own session named `name`, whose tab starts in
/// `cwd` with your shell, starting the daemon where none runs, and says
/// which it made; or why it made none, as where the name is taken.
pub fn made(name: &SessionName, cwd: &Directory) -> Result<OwnKey, String> {
    let folder = lunetad::folder::find()
        .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))?;
    let request = NewOwnRequest {
        on: None,
        name: name.as_str().to_owned(),
        cwd: cwd.as_str().to_owned(),
        command: Vec::new(),
    };
    let making = async {
        let mut daemon = daemon::current(&folder).await?.daemon;
        let made = daemon.new_own(request).await.map_err(|status| status.message().to_owned())?;
        Ok::<_, String>(made.into_inner())
    };
    let made = runtime.block_on(async { tokio::time::timeout(MAKING, making).await });
    let made = made.map_err(|_| format!("lunetad made no session in {}s", MAKING.as_secs()))??;
    if made.made_at.is_some() {
        let session = OwnSession { id: made.session, made_at: made.made_at };
        return OwnKey::of(&session).map_err(|why| format!("lunetad made {why}"));
    }
    // A daemon older than `made_at`: the server that made it says, where it
    // still holds a session of the id by the name.
    let id = OwnId::new(made.session).ok_or("lunetad made an own session of id 0")?;
    match named(name)? {
        Some(key) if key.id == id => Ok(key),
        _ => Err(format!("own session {id}, {name}, no longer runs")),
    }
}

#[cfg(test)]
mod tests {
    use lunetad::mux::own::OwnMade;

    use super::*;

    fn key(id: u64, made: u64) -> OwnKey {
        OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(made * 1_000_000_000) }
    }

    /// luneta goes on in the newest own session, but the one it runs in and
    /// those it lost its client of since you last went anywhere, so that two
    /// whose clients it keeps losing do not have it go back and forth: with
    /// all passed, it goes on in none.
    #[test]
    fn luneta_goes_on_in_the_newest_own_session_it_did_not_pass() {
        let sessions = [key(1, 10), key(2, 30), key(3, 20)];
        let newest = |held, passed: &[OwnKey]| newest_of(sessions.into_iter(), held, passed);
        assert_eq!(newest(None, &[]), Some(key(2, 30)));
        assert_eq!(newest(Some(key(2, 30)), &[]), Some(key(3, 20)));
        assert_eq!(newest(None, &[key(2, 30)]), Some(key(3, 20)));
        assert_eq!(newest(None, &[key(2, 30), key(3, 20)]), Some(key(1, 10)));
        assert_eq!(newest(Some(key(1, 10)), &[key(2, 30), key(3, 20)]), None);
        assert_eq!(newest(None, &[key(2, 20)]), Some(key(2, 30)), "of the id made at another time");
    }
}
