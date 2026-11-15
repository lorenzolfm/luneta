use std::fmt;
use std::path::Path;
use std::time::Duration;

use lunetad::api::{NewOwnRequest, OwnSession};
use lunetad::mux::own::{self, server, OwnId, OwnKey};
use lunetad::{log, Directory, SessionName};
use tonic::{Code, Status};

use crate::daemon;

const MAKING: Duration = Duration::from_secs(20);

pub fn holding() -> Result<Option<OwnKey>, String> {
    own::here().map(|pane| pane.map(|pane| pane.session))
}

pub fn refusal(session: OwnKey, name: &SessionName) -> Option<String> {
    match holding() {
        Ok(held) => (held == Some(session))
            .then(|| format!("luneta runs in {name}, and goes nowhere it shows itself")),
        Err(why) => Some(format!(
            "{why}, so luneta may run in {name}, and goes nowhere it shows itself"
        )),
    }
}

pub fn named(name: &SessionName) -> Result<Option<OwnKey>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.key()))
}

pub fn directory(dir: &Path) -> Result<Directory, String> {
    let cwd = dir.to_str().and_then(|dir| Directory::parse(dir.to_owned()).ok());
    cwd.ok_or_else(|| format!("{} is no directory an own session starts in", dir.display()))
}

pub fn newest(passed: &[OwnKey]) -> Option<OwnKey> {
    let sessions = server::sessions()
        .inspect_err(|failure| log::say(&format!("no own session to go on in: {failure}")))
        .ok()?;
    let held = holding()
        .inspect_err(|why| log::say(&format!("no own session to go on in: {why}")))
        .ok()?;
    newest_of(sessions.iter().map(server::Listing::key), held, passed)
}

fn newest_of(
    sessions: impl Iterator<Item = OwnKey>,
    held: Option<OwnKey>,
    passed: &[OwnKey],
) -> Option<OwnKey> {
    let free = sessions.filter(|key| Some(*key) != held && !passed.contains(key));
    free.max_by_key(|key| (key.made, key.id))
}

#[derive(Debug)]
pub enum Unmade {
    Taken(String),
    Failed(String),
}

impl fmt::Display for Unmade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unmade::Taken(said) | Unmade::Failed(said) => f.write_str(said),
        }
    }
}

impl From<Unmade> for String {
    fn from(unmade: Unmade) -> Self {
        unmade.to_string()
    }
}

impl From<String> for Unmade {
    fn from(why: String) -> Self {
        Unmade::Failed(why)
    }
}

impl From<&str> for Unmade {
    fn from(why: &str) -> Self {
        Unmade::Failed(why.to_owned())
    }
}

fn unmade(status: &Status) -> Unmade {
    let said = status.message().to_owned();
    match status.code() {
        Code::AlreadyExists => Unmade::Taken(said),
        _ => Unmade::Failed(said),
    }
}

pub fn made(name: &SessionName, cwd: &Directory) -> Result<OwnKey, Unmade> {
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
        let made = daemon.new_own(request).await.map_err(|status| unmade(&status))?;
        Ok::<_, Unmade>(made.into_inner())
    };
    let made = runtime.block_on(async { tokio::time::timeout(MAKING, making).await });
    let made = made.map_err(|_| format!("lunetad made no session in {}s", MAKING.as_secs()))??;
    if made.made_at.is_some() {
        let session = OwnSession { id: made.session, made_at: made.made_at };
        return Ok(OwnKey::of(&session).map_err(|why| format!("lunetad made {why}"))?);
    }
    let id = OwnId::new(made.session).ok_or("lunetad made an own session of id 0")?;
    match named(name)? {
        Some(key) if key.id == id => Ok(key),
        _ => Err(Unmade::Failed(format!("own session {id}, {name}, no longer runs"))),
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
