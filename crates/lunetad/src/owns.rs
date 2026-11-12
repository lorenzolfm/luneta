//! The own sessions, of luneta's own multiplexer, as their server lists them,
//! with their panes and clients. The server says each change as it makes it,
//! so the daemon follows its `Watch` and asks it nothing on a sweep. While no
//! server runs, the daemon waits for its socket to turn up in luneta's
//! runtime folder.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::RecursiveMode;

use crate::core::{self, Command};
use crate::mux::own::server::{self, Listing, SOCKET};
use crate::panes;
use crate::watch::{self, Troubles};

/// Has the core told the own sessions as their server lists them, and again
/// each time they change, in a thread of its own, and says on `running`
/// whether one runs. Where no server runs, or once it ends, the core hears
/// that none does, and the thread waits for a server's socket to turn up in
/// its folder, or for a `sweep` to pass, which is all it has where the folder
/// cannot be watched. The thread ends once the core has.
pub fn follow(
    core: SyncSender<Command>,
    running: Arc<AtomicBool>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("owns".to_owned()).spawn(move || {
        let (wake, woken) = mpsc::sync_channel(1);
        let mut watcher = None;
        let mut watching = Troubles::default();
        let mut troubles = Troubles::default();
        let folder: Option<PathBuf> = match server::socket() {
            Ok(socket) => socket.parent().map(Path::to_owned),
            Err(failure) => {
                troubles.say(format!("the own sessions are not followed: {failure}"));
                None
            },
        };
        loop {
            if let Some(folder) = &folder
                && watcher.is_none()
            {
                let mode = RecursiveMode::NonRecursive;
                watcher = watch::watching(folder, mode, is_socket, &wake, &mut watching);
                watching.done();
            }
            let followed = server::followed(|listed, unread| {
                running.store(!listed.is_empty() || !unread.is_empty(), Ordering::Relaxed);
                for why in unread {
                    troubles.say(format!("the own sessions: {why}"));
                }
                let owns = owns(listed, &mut troubles);
                troubles.done();
                core.send(Command::Owns(owns)).is_ok()
            });
            if let Err(failure) = followed {
                troubles.say(format!("the own sessions: {failure}"));
                troubles.done();
            }
            running.store(false, Ordering::Relaxed);
            if core.send(Command::Owns(Vec::new())).is_err() {
                return;
            }
            match woken.recv_timeout(sweep) {
                Ok(()) => watch::settle(&woken),
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

/// Whether a change at `path`, in luneta's runtime folder, can be the
/// server's socket coming or going.
fn is_socket(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == SOCKET)
}

/// The own sessions `listed`, as the core keeps them: their terminals, as
/// the daemon reads a session's panes. One whose panes do not read is left
/// out, and `troubles` says why.
fn owns(listed: Vec<Listing>, troubles: &mut Troubles) -> Vec<core::Own> {
    let own = |Listing { id, name, laid, clients }: Listing| match panes::terminals(laid) {
        Ok(panes) => Some(core::Own { id, name, panes, clients }),
        Err(why) => {
            troubles.say(format!("the panes of own session {id}: {why}"));
            None
        },
    };
    listed.into_iter().filter_map(own).collect()
}
