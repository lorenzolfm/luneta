//! The own sessions of this machine, as luneta goes to them: never the one
//! luneta runs in, whose client would show luneta in itself, and each key
//! typed in it go round again.

use lunetad::mux::own::{self, OwnId};
use lunetad::SessionName;

/// The own session luneta runs in, where it runs in a pane of one.
fn holding() -> Option<OwnId> {
    own::here().map(|(session, _)| session.id)
}

/// Why luneta would not go to the own session `id`: it runs in a pane of it.
pub fn refusal(id: OwnId, name: &SessionName) -> Option<String> {
    (holding() == Some(id))
        .then(|| format!("luneta runs in {name}, and goes nowhere it shows itself"))
}
