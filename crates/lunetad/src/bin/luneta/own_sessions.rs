//! The own sessions of this machine, as luneta goes to them: made by the
//! daemon, which alone starts their server, and never the one luneta runs
//! in, whose client would show luneta in itself, and each key typed in it go
//! round again.

use std::time::{Duration, SystemTime};

use lunetad::api::NewOwnRequest;
use lunetad::mux::own::{self, server, OwnId};
use lunetad::{log, Directory, SessionName};

use crate::daemon;

/// How long luneta waits for the daemon to make an own session: for it to
/// start, where none runs, and for the server to, as `server::made` waits.
const MAKING: Duration = Duration::from_secs(20);

/// The own session luneta runs in, where it runs in a pane of one.
fn holding() -> Option<OwnId> {
    own::here().map(|(session, _)| session.id)
}

/// Why luneta would not go to the own session `id`: it runs in a pane of it.
pub fn refusal(id: OwnId, name: &SessionName) -> Option<String> {
    (holding() == Some(id))
        .then(|| format!("luneta runs in {name}, and goes nowhere it shows itself"))
}

/// The own session named `name`, by its id, where one runs by it.
pub fn named(name: &SessionName) -> Result<Option<OwnId>, String> {
    let named = server::named(name).map_err(|failure| failure.to_string())?;
    Ok(named.map(|own| own.id()))
}

/// An own session luneta can go on in, as `newest` finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Newest {
    pub id: OwnId,
    /// When its server made it.
    pub made: SystemTime,
}

/// The newest own session that runs, by when its server made it, but the
/// one luneta runs in: none where none runs, or the server does not answer,
/// which the log says.
pub fn newest() -> Option<Newest> {
    let sessions = server::sessions()
        .inspect_err(|failure| log::say(&format!("no own session to go on in: {failure}")))
        .ok()?;
    let held = holding();
    let free = sessions.into_iter().filter(|session| Some(session.id) != held);
    let newest = free.max_by_key(|session| (session.made, session.id))?;
    Some(Newest { id: newest.id, made: newest.made })
}

/// Has the daemon make an own session named `name`, whose tab starts in
/// `cwd` with your shell, starting the daemon where none runs, and says its
/// id; or why it made none, as where the name is taken.
pub fn made(name: &SessionName, cwd: &Directory) -> Result<OwnId, String> {
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
    OwnId::new(made.session).ok_or_else(|| "lunetad made an own session of id 0".to_owned())
}
