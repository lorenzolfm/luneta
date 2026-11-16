use std::fmt;
use std::time::Duration;

use lunetad::api::{NewOwnRequest, OwnSession};
use lunetad::mux::own::{server, OwnId, OwnKey};
use lunetad::{Directory, SessionName};
use tonic::{Code, Status};

use crate::daemon;

const MAKING: Duration = Duration::from_secs(20);

pub fn named(name: &SessionName) -> Result<Option<OwnKey>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.key()))
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

pub fn made(name: &SessionName, cwd: &Directory) -> Result<OwnKey, Unmade> {
    let request = NewOwnRequest {
        on: None,
        name: name.as_str().to_owned(),
        cwd: cwd.as_str().to_owned(),
        command: Vec::new(),
        tab: None,
    };
    let made = asked(request)?;
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
