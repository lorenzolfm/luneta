use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lune_domain::mux::TerminalKey;
use notify::RecursiveMode;

use crate::core;
use crate::mux::own::server::{self, Followed, SOCKET};
use crate::layouts;
use crate::watch::{self, Troubles};

const UNREACHED_TRIES: u32 = 50;

const UNREACHED_PAUSE: Duration = Duration::from_millis(20);

pub fn follow(
    core: SyncSender<core::Command>,
    layouts: SyncSender<layouts::Command>,
    (wake, woken): (SyncSender<()>, Receiver<()>),
    running: Arc<AtomicBool>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("owns".to_owned()).spawn(move || {
        let mut watcher = None;
        let mut watching = Troubles::default();
        let mut troubles = Troubles::default();
        let mut told_mux = BTreeMap::new();
        let socket = match server::socket() {
            Ok(socket) => Some(socket),
            Err(failure) => {
                troubles.say(format!("lune-mux's terminals are not followed: {failure}"));
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
                    troubles.say(format!("lune-mux's terminals: {why}"));
                }
                troubles.done();
                if !tell_mux(&core, &mut told_mux, &terminals.terminals) {
                    return false;
                }
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
                    troubles.say(format!("terminals are not followed, as lune-mux ended: {why}"));
                    troubles.done();
                    Some(layouts::Command::Crashed)
                },
                Ok(Followed::LetGo) => return,
                Err(failure) => {
                    troubles.say(format!("terminals are not followed: {failure}"));
                    troubles.done();
                    Some(layouts::Command::Crashed)
                },
            };
            if !unreached_now {
                unreached = 0;
            }
            if let Some(unlisted) = unlisted {
                if !tell_mux(&core, &mut told_mux, &BTreeMap::new()) {
                    return;
                }
                if layouts.send(unlisted).is_err() {
                    return;
                }
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

fn tell_mux(
    core: &SyncSender<core::Command>,
    told: &mut BTreeMap<TerminalKey, core::MuxTerminal>,
    terminals: &BTreeMap<TerminalKey, server::Terminal>,
) -> bool {
    let now: BTreeMap<TerminalKey, core::MuxTerminal> = terminals
        .iter()
        .filter_map(|(&key, terminal)| Some((key, core::MuxTerminal::of(terminal)?)))
        .collect();
    if now == *told {
        return true;
    }
    if core.send(core::Command::MuxTerminals(now.clone())).is_err() {
        return false;
    }
    *told = now;
    true
}

fn is_socket(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == SOCKET)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::mpsc;

    use lune_domain::mux::OwnMade;
    use lune_domain::projects::Size;
    use lune_protocol::program_status::{AppName, ProgramPhase, ProgramStatus, ProgramStatuses};

    use super::*;
    use crate::mux::own::server::{Foreground, Is};
    use crate::process::AgentPid;
    use crate::Directory;

    fn listed(
        title: &str,
        size: Option<Size>,
        statuses: ProgramStatuses,
    ) -> BTreeMap<TerminalKey, server::Terminal> {
        let key = TerminalKey { id: NonZeroU32::new(1).unwrap(), made: OwnMade::from_nanos(5) };
        let terminal = server::Terminal {
            tag: b"t".to_vec(),
            size,
            cwd: Some(Directory::parse("/w".to_owned()).unwrap()),
            title: title.to_owned(),
            is: Is::Running {
                root: AgentPid::new(10).unwrap(),
                front: Some(Foreground {
                    pid: AgentPid::new(20).unwrap(),
                    argv: vec!["claude".to_owned()],
                }),
                doing: None,
            },
            statuses,
        };
        BTreeMap::from([(key, terminal)])
    }

    fn osc(phase: ProgramPhase) -> ProgramStatuses {
        ProgramStatuses {
            root: Some(ProgramStatus {
                phase,
                app: Some(AppName::parse("claude-code").unwrap()),
                title: None,
                at: None,
            }),
            children: Vec::new(),
        }
    }

    #[test]
    fn a_terminal_whose_title_only_cycles_spinner_glyphs_sends_no_mux_terminals() {
        let (core, commands) = mpsc::sync_channel(8);
        let mut told = BTreeMap::new();

        for title in ["⠋ Working", "⠙ Working", "✳ Working"] {
            assert!(tell_mux(&core, &mut told, &listed(title, None, ProgramStatuses::default())));
            assert!(commands.try_recv().is_err(), "{title}");
        }

        let working = osc(ProgramPhase::Working { progress: None });
        assert!(tell_mux(&core, &mut told, &listed("⠋ Working", None, working.clone())));
        assert!(matches!(commands.try_recv(), Ok(core::Command::MuxTerminals(sent)) if sent.len() == 1));

        let resized = Size::of(120, 40).unwrap();
        for title in ["⠙ Working", "⠹ Working", "✳ Working"] {
            assert!(tell_mux(&core, &mut told, &listed(title, Some(resized), working.clone())));
            assert!(commands.try_recv().is_err(), "{title}");
        }

        let done = osc(ProgramPhase::Done { message: None });
        assert!(tell_mux(&core, &mut told, &listed("✳ Working", Some(resized), done)));
        assert!(matches!(commands.try_recv(), Ok(core::Command::MuxTerminals(sent)) if sent.len() == 1));

        assert!(tell_mux(&core, &mut told, &BTreeMap::new()));
        assert!(matches!(commands.try_recv(), Ok(core::Command::MuxTerminals(sent)) if sent.is_empty()));

        assert!(tell_mux(&core, &mut told, &BTreeMap::new()));
        assert!(commands.try_recv().is_err());
    }
}
