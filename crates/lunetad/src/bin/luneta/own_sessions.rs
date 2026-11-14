use std::time::Duration;

use lunetad::api::{NewOwnRequest, OwnSession};
use lunetad::mux::own::{self, server, OwnId, OwnKey};
use lunetad::{log, Directory, SessionName};

use crate::daemon;

const MAKING: Duration = Duration::from_secs(20);

fn holding() -> Option<OwnKey> {
    own::here().map(|pane| pane.session)
}

pub fn refusal(session: OwnKey, name: &SessionName) -> Option<String> {
    (holding() == Some(session))
        .then(|| format!("luneta runs in {name}, and goes nowhere it shows itself"))
}

pub fn named(name: &SessionName) -> Result<Option<OwnKey>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.key()))
}

pub fn newest(passed: &[OwnKey]) -> Option<OwnKey> {
    let sessions = server::sessions()
        .inspect_err(|failure| log::say(&format!("no own session to go on in: {failure}")))
        .ok()?;
    newest_of(sessions.iter().map(server::Listing::key), holding(), passed)
}

fn newest_of(
    sessions: impl Iterator<Item = OwnKey>,
    held: Option<OwnKey>,
    passed: &[OwnKey],
) -> Option<OwnKey> {
    let free = sessions.filter(|key| Some(*key) != held && !passed.contains(key));
    free.max_by_key(|key| (key.made, key.id))
}

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
