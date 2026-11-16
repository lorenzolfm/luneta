use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::RecursiveMode;

use crate::core::{self, Command};
use crate::mux::own::server::{self, Followed, Listing, SOCKET};
use crate::panes;
use crate::views;
use crate::watch::{self, Troubles};

const UNREACHED_TRIES: u32 = 50;

const UNREACHED_PAUSE: Duration = Duration::from_millis(20);

pub fn follow(
    core: SyncSender<Command>,
    views: SyncSender<views::Command>,
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
                troubles.say(format!("the own sessions are not followed: {failure}"));
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
            let followed = server::followed(|listed, unread, terminals| {
                let held = !listed.is_empty() || !terminals.terminals.is_empty();
                running.store(held || !unread.is_empty(), Ordering::Relaxed);
                if views.send(views::Command::Listed(terminals)).is_err() {
                    return false;
                }
                for why in unread {
                    troubles.say(format!("the own sessions: {why}"));
                }
                let owns = owns(listed, &mut troubles);
                troubles.done();
                core.send(Command::Owns(owns)).is_ok()
            });
            running.store(false, Ordering::Relaxed);
            let listens_soon = socket.as_deref().is_some_and(Path::exists);
            let unreached_now = matches!(followed, Ok(Followed::Unreached));
            let unlisted = match followed {
                Ok(Followed::Unreached) if listens_soon && unreached < UNREACHED_TRIES => None,
                Ok(Followed::Unreached) => Some(views::Command::Unreached),
                Ok(Followed::Emptied { since }) => Some(views::Command::Emptied { since }),
                Ok(Followed::LetGo) => return,
                Err(failure) => {
                    troubles.say(format!("the own sessions: {failure}"));
                    troubles.done();
                    Some(views::Command::Crashed)
                },
            };
            if !unreached_now {
                unreached = 0;
            }
            if let Some(unlisted) = unlisted
                && views.send(unlisted).is_err()
            {
                return;
            }
            if core.send(Command::Owns(Vec::new())).is_err() {
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

fn owns(listed: Vec<Listing>, troubles: &mut Troubles) -> Vec<core::Own> {
    let own = |Listing { id, made, name, laid, clients, doings }: Listing| {
        match panes::terminals(laid) {
            Ok(panes) => Some(core::Own { id, made, name, panes, clients, doings }),
            Err(why) => {
                troubles.say(format!("the panes of own session {id}: {why}"));
                None
            },
        }
    };
    listed.into_iter().filter_map(own).collect()
}
