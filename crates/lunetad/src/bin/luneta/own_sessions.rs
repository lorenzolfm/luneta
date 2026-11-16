use std::fmt;
use std::path::Path;
use std::time::Duration;

use lunetad::api::{NewOwnRequest, On, OwnSession};
use lunetad::mux::own::attach::To;
use lunetad::mux::own::{self, server, OwnId, OwnKey};
use lunetad::{Destination, Directory, SessionName};
use tonic::{Code, Status};

use crate::daemon;

const MAKING: Duration = Duration::from_secs(20);

pub fn runs_in() -> Result<Option<OwnKey>, String> {
    own::here().map(|pane| pane.map(|pane| pane.session))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elsewhere(To);

impl Elsewhere {
    #[must_use]
    pub fn to(self) -> To {
        self.0
    }
}

pub fn elsewhere(
    runs_in: Option<OwnKey>,
    to: To,
    name: impl fmt::Display,
) -> Result<Elsewhere, String> {
    if runs_in == Some(to.session()) {
        return Err(format!("luneta runs in {name}, and goes nowhere it shows itself"));
    }
    Ok(Elsewhere(to))
}

pub fn free<T>(
    runs_in: Option<OwnKey>,
    sessions: impl IntoIterator<Item = (OwnKey, T)>,
) -> impl Iterator<Item = (Elsewhere, T)> {
    let free = sessions.into_iter().filter(move |(key, _)| Some(*key) != runs_in);
    free.map(|(key, session)| (Elsewhere(To::Session(key)), session))
}

pub fn named(name: &SessionName) -> Result<Option<OwnKey>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.key()))
}

pub fn directory(dir: &Path) -> Result<Directory, String> {
    let cwd = dir.to_str().and_then(|dir| Directory::parse(dir.to_owned()).ok());
    cwd.ok_or_else(|| format!("{} is no directory an own session starts in", dir.display()))
}

pub fn newest(
    runs_in: Option<OwnKey>,
    passed: &[OwnKey],
) -> Result<Option<Elsewhere>, String> {
    let sessions = server::sessions().map_err(|failure| failure.to_string())?;
    Ok(newest_of(sessions.iter().map(server::Listing::key), runs_in, passed))
}

fn newest_of(
    sessions: impl Iterator<Item = OwnKey>,
    runs_in: Option<OwnKey>,
    passed: &[OwnKey],
) -> Option<Elsewhere> {
    let unpassed = sessions.filter(|key| !passed.contains(key)).map(|key| (key, key));
    let newest = free(runs_in, unpassed).max_by_key(|(_, key)| (key.made, key.id));
    newest.map(|(to, _)| to)
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

pub struct First<'a> {
    pub tab: &'a str,
    pub command: &'a [String],
}

pub fn made(name: &SessionName, cwd: &Directory) -> Result<OwnKey, Unmade> {
    made_here(name, cwd, None)
}

pub fn made_here(
    name: &SessionName,
    cwd: &Directory,
    first: Option<First<'_>>,
) -> Result<OwnKey, Unmade> {
    let made = asked(new_own(None, name, cwd, first))?;
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

pub fn made_there(
    destination: &Destination,
    name: &SessionName,
    cwd: &Directory,
    first: Option<First<'_>>,
) -> Result<(), Unmade> {
    let on = On { destination: destination.to_string() };
    asked(new_own(Some(on), name, cwd, first)).map(drop)
}

fn new_own(
    on: Option<On>,
    name: &SessionName,
    cwd: &Directory,
    first: Option<First<'_>>,
) -> NewOwnRequest {
    NewOwnRequest {
        on,
        name: name.as_str().to_owned(),
        cwd: cwd.as_str().to_owned(),
        command: first.as_ref().map_or_else(Vec::new, |first| first.command.to_vec()),
        tab: first.map(|first| first.tab.to_owned()),
    }
}

fn asked(request: NewOwnRequest) -> Result<lunetad::api::NewOwnResponse, Unmade> {
    let folder = lunetad::folder::find()
        .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))?;
    let making = async {
        let mut daemon = daemon::current(&folder).await?.daemon;
        let made = daemon.new_own(request).await.map_err(|status| unmade(&status))?;
        Ok::<_, Unmade>(made.into_inner())
    };
    let made = runtime.block_on(async { tokio::time::timeout(MAKING, making).await });
    made.map_err(|_| format!("lunetad made no session in {}s", MAKING.as_secs()))?
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use lunetad::mux::own::OwnMade;

    use super::*;

    fn key(id: u64, made: u64) -> OwnKey {
        OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(made * 1_000_000_000) }
    }

    #[test]
    fn luneta_goes_anywhere_but_into_the_session_it_runs_in() {
        use lunetad::mux::own::OwnPane;

        let name = SessionName::parse("project").unwrap();
        let (here, there) = (key(1, 10), key(2, 20));
        let into = [
            To::Session(here),
            To::Tab { session: here, tab: NonZeroU32::MIN },
            To::Pane(OwnPane::new(here, 3).unwrap()),
        ];
        let refused = "luneta runs in project, and goes nowhere it shows itself";
        for to in into {
            assert_eq!(elsewhere(Some(here), to, &name), Err(refused.to_owned()), "{to:?}");
            assert_eq!(elsewhere(Some(there), to, &name).map(Elsewhere::to), Ok(to));
            assert_eq!(elsewhere(None, to, &name).map(Elsewhere::to), Ok(to));
        }
    }

    #[test]
    fn luneta_goes_on_in_the_newest_own_session_it_did_not_pass() {
        let sessions = [key(1, 10), key(2, 30), key(3, 20)];
        let newest = |runs_in, passed: &[OwnKey]| {
            newest_of(sessions.into_iter(), runs_in, passed).map(|to| to.to().session())
        };
        assert_eq!(newest(None, &[]), Some(key(2, 30)));
        assert_eq!(newest(Some(key(2, 30)), &[]), Some(key(3, 20)));
        assert_eq!(newest(None, &[key(2, 30)]), Some(key(3, 20)));
        assert_eq!(newest(None, &[key(2, 30), key(3, 20)]), Some(key(1, 10)));
        assert_eq!(newest(Some(key(1, 10)), &[key(2, 30), key(3, 20)]), None);
        assert_eq!(newest(None, &[key(2, 20)]), Some(key(2, 30)), "of the id made at another time");
    }
}
