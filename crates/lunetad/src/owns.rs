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
            let followed = server::followed(|listed, unread, terminals| {
                let held = !listed.is_empty() || !terminals.is_empty();
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
            let emptied = match &followed {
                Ok(Followed::Unreached) => None,
                Ok(Followed::Emptied) => Some(true),
                Ok(Followed::LetGo) => Some(false),
                Err(failure) => {
                    troubles.say(format!("the own sessions: {failure}"));
                    troubles.done();
                    Some(false)
                },
            };
            running.store(false, Ordering::Relaxed);
            if let Some(emptied) = emptied
                && views.send(views::Command::Unlisted { emptied }).is_err()
            {
                return;
            }
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
