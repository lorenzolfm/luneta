use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::RecursiveMode;

use crate::mux::own::server::{self, Followed, SOCKET};
use crate::layouts;
use crate::watch::{self, Troubles};

const UNREACHED_TRIES: u32 = 50;

const UNREACHED_PAUSE: Duration = Duration::from_millis(20);

pub fn follow(
    layouts: SyncSender<layouts::Command>,
    (wake, woken): (SyncSender<()>, Receiver<()>),
    running: Arc<AtomicBool>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("owns".to_owned()).spawn(move || {
        let mut watcher = None;
        let mut watching = Troubles::default();
        let mut troubles = Troubles::default();
        let socket = match server::socket() {
            Ok(socket) => Some(socket),
            Err(failure) => {
                troubles.say(format!("luneta-mux's terminals are not followed: {failure}"));
                None
            },
        };
        let folder: Option<PathBuf> = socket.as_deref().and_then(Path::parent).map(Path::to_owned);
        let mut unreached = 0;
        loop {
            if let Some(folder) = &folder
                && watcher.is_none()
            {
                let mode = RecursiveMode::NonRecursive;
                watcher = watch::watching(folder, mode, is_socket, &wake, &mut watching);
                watching.done();
            }
            let followed = server::followed(|unread, terminals| {
                let held = !terminals.terminals.is_empty();
                running.store(held || !unread.is_empty(), Ordering::Relaxed);
                for why in unread {
                    troubles.say(format!("luneta-mux's terminals: {why}"));
                }
                troubles.done();
                layouts.send(layouts::Command::Listed(terminals)).is_ok()
            });
            running.store(false, Ordering::Relaxed);
            let listens_soon = socket.as_deref().is_some_and(Path::exists);
            let unreached_now = matches!(followed, Ok(Followed::Unreached { .. }));
            let retried = listens_soon && unreached < UNREACHED_TRIES;
            let unlisted = match followed {
                Ok(Followed::Unreached { .. }) if retried => None,
                Ok(Followed::Unreached { at }) => Some(layouts::Command::Unreached { since: at }),
                Ok(Followed::Unmatched) => Some(layouts::Command::Crashed),
                Ok(Followed::Newer(why)) => {
                    troubles.say(why);
                    troubles.done();
                    None
                },
                Ok(Followed::Emptied { since }) => Some(layouts::Command::Emptied { since }),
                Ok(Followed::Lost(why)) => {
                    troubles.say(format!("luneta-mux's terminals: luneta-mux: {why}"));
                    troubles.done();
                    Some(layouts::Command::Crashed)
                },
                Ok(Followed::LetGo) => return,
                Err(failure) => {
                    troubles.say(format!("luneta-mux's terminals: {failure}"));
                    troubles.done();
                    Some(layouts::Command::Crashed)
                },
            };
            if !unreached_now {
                unreached = 0;
            }
            if let Some(unlisted) = unlisted
                && layouts.send(unlisted).is_err()
            {
                return;
            }
            if unreached_now && listens_soon && unreached < UNREACHED_TRIES {
                unreached += 1;
                thread::sleep(UNREACHED_PAUSE);
                continue;
            }
            match woken.recv_timeout(sweep) {
                Ok(()) => {
                    watch::settle(&woken);
                    unreached = 0;
                },
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

fn is_socket(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == SOCKET)
}
