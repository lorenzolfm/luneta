//! A key of the menu's Review group, for the review an agent runs: the daemon
//! of the agent's machine keeps it, so it is asked through the daemon luneta
//! watches, as what `away` asks of another machine is.

use std::sync::mpsc::Sender;
use std::thread;

use lunetad::api::{MoveReviewRequest, On};
use lunetad::reviews::Move;

use crate::away;
use crate::commands::{self, causes};
use crate::daemon::{self, Reach};
use crate::host::Event;
use crate::machines::Machine;

/// Has the daemon of `on` move the review of the agent `pid` as `does` says,
/// on a thread of its own, at once: what comes of it shows on the agent's row,
/// and why it did not, in the notes.
pub fn moved(on: Machine, pid: u32, does: Move, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        if let Err(why) = asked(&on, pid, does) {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(format!("review: {why}")));
        }
    });
}

/// Asks it, and says why it did not, within what a thread of luneta's waits.
fn asked(on: &Machine, pid: u32, does: Move) -> Result<(), String> {
    let folder = lunetad::folder::find()
        .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
    let on = match on {
        Machine::Local => None,
        Machine::Remote(destination) => Some(On { destination: destination.to_string() }),
    };
    let request = MoveReviewRequest { on, pid, does: does.to_api().into() };
    commands::runtime()?.block_on(async {
        let asking = async {
            let mut daemon = daemon::reach(&folder, Reach::Running).await?;
            let answer = daemon.move_review(request).await;
            answer.map(drop).map_err(|status| {
                if status.message().is_empty() { causes(&status) } else { status.message().to_owned() }
            })
        };
        let late = || format!("lunetad did not answer in {}s", away::ANSWER.as_secs());
        tokio::time::timeout(away::ANSWER, asking).await.map_err(|_| late())?
    })
}
