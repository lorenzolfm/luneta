use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use notify::RecursiveMode;

use crate::api;
use crate::core::{self, Addressed};
use crate::shells::Doing;
use crate::watch::{self, Troubles};
use crate::mux::{CacheFolder, Failure, Id, Laid, Metadata, Mux, SessionName, Zellij};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: u32,
    pub tab: u32,
    pub tab_id: u32,
    pub tab_name: String,
    pub cwd: Option<Said>,
    pub command: Option<Said>,
    pub titled: Option<Titled>,
    pub held: bool,
}

impl Pane {
    #[must_use]
    pub fn to_api(&self, doing: Option<&Doing>) -> api::Pane {
        api::Pane {
            id: self.id,
            tab: self.tab,
            tab_name: self.tab_name.clone(),
            cwd: self.cwd.clone().map(String::from),
            command: self.command.clone().map(String::from),
            titled: self.titled.as_ref().map(Titled::to_api),
            shell: doing.map(Doing::to_api),
            tab_id: Some(self.tab_id),
            held: self.held,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Titled {
    pub topic: Option<Said>,
}

impl Titled {
    pub(crate) fn to_api(&self) -> api::Titled {
        api::Titled { topic: self.topic.clone().map(String::from) }
    }
}

pub(crate) fn terminals(laid: Vec<Laid>) -> Result<Vec<Pane>, String> {
    let panes: Vec<Pane> = laid
        .into_iter()
        .filter(|laid| !laid.is_suppressed)
        .filter_map(|laid| {
            Some(Pane {
                id: laid.pane.terminal()?,
                tab: laid.tab_position,
                tab_id: laid.tab_id.0,
                tab_name: laid.tab_name,
                cwd: laid.cwd,
                command: laid.command,
                titled: titled(&laid.title),
                held: laid.is_held,
            })
        })
        .collect();
    let mut ids = HashSet::new();
    if let Some(twice) = panes.iter().find(|pane| !ids.insert(pane.id)) {
        return Err(format!("pane {} comes twice", twice.id));
    }
    Ok(panes)
}

const GENERIC_TITLE: &str = "Claude Code";

pub(crate) fn titled(title: &str) -> Option<Titled> {
    let mut chars = title.trim().chars();
    let glyph = chars.next()?;
    let agent = matches!(glyph, '✳' | '◐' | '◑' | '✢' | '✶' | '✻' | '✽' | '·')
        || ('\u{2800}'..='\u{28ff}').contains(&glyph);
    let topic = Some(chars.as_str().trim_start()).filter(|topic| *topic != GENERIC_TITLE);
    agent.then(|| Titled { topic: topic.map(str::to_owned).and_then(Said::new) })
}

pub fn keep(panes: &mut [Pane], listed: &[Pane]) {
    for pane in panes {
        let Some(was) = listed.iter().find(|was| was.id == pane.id) else {
            continue;
        };
        pane.cwd = pane.cwd.take().or_else(|| was.cwd.clone());
        pane.command = pane.command.take().or_else(|| was.command.clone());
    }
}

fn list(session: &SessionName) -> Result<Vec<Pane>, String> {
    let laid = Mux::Zellij(session).panes_listed()?;
    terminals(laid).map_err(|why| format!("zellij listed what does not read: {why}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signs {
    metadata: Option<Metadata>,
    told: u64,
}

struct Asked {
    at: Instant,
    signs: Signs,
}

pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    metadata_folder: Option<CacheFolder>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("panes".to_owned()).spawn(move || {
        let mut asked: HashMap<Id, Asked> = HashMap::new();
        let mut troubles: HashMap<Id, Troubles> = HashMap::new();
        let mut watcher = None;
        let mut watching = Troubles::default();
        'round: loop {
            if let Some(folder) = metadata_folder.as_ref().map(CacheFolder::path)
                && watcher.is_none()
            {
                let wanted = |path: &Path| Zellij::is_metadata(path);
                let mode = RecursiveMode::Recursive;
                watcher = watch::watching(folder, mode, wanted, &wake, &mut watching);
                watching.done();
            }
            let Some(sessions) = core::ask(&core, core::Command::Sessions) else {
                return;
            };
            let started = sessions.iter().map(|session| session.id.started());
            if let Some(wait) = Zellij::newborn(started, SystemTime::now()) {
                thread::sleep(wait);
                continue;
            }
            let running = |id: &Id| sessions.iter().any(|session| session.id == *id);
            asked.retain(|id, _| running(id));
            troubles.retain(|id, _| running(id));
            let now = Instant::now();
            let mut asking: Vec<(Addressed, Signs)> = sessions
                .into_iter()
                .filter_map(|session| {
                    let troubles = troubles.entry(session.id).or_default();
                    let metadata = metadata_folder
                        .as_ref()
                        .and_then(|folder| written(folder, &session.name, troubles));
                    let signs = Signs { metadata, told: session.told };
                    due(asked.get(&session.id), signs, now, sweep).then_some((session, signs))
                })
                .collect();
            asking.sort_by_key(|(session, _)| session.listed);
            for (session, signs) in asking {
                let troubles = troubles.entry(session.id).or_default();
                if ask(&core, &session, troubles).is_none() {
                    return;
                }
                troubles.done();
                asked.insert(session.id, Asked { at: Instant::now(), signs });
                if woken.try_recv().is_ok() {
                    watch::settle(&woken);
                    continue 'round;
                }
            }
            match woken.recv_timeout(next(asked.values(), sweep, Instant::now())) {
                Ok(()) => watch::settle(&woken),
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

fn written(folder: &CacheFolder, name: &SessionName, troubles: &mut Troubles) -> Option<Metadata> {
    folder.metadata(name).unwrap_or_else(|error| {
        troubles.say(format!("the metadata of {name}: {error}"));
        None
    })
}

fn due(asked: Option<&Asked>, signs: Signs, now: Instant, sweep: Duration) -> bool {
    asked.is_none_or(|asked| {
        asked.signs != signs || now.saturating_duration_since(asked.at) >= sweep
    })
}

fn next<'a>(asked: impl Iterator<Item = &'a Asked>, sweep: Duration, now: Instant) -> Duration {
    let left = |asked: &Asked| (asked.at + sweep).saturating_duration_since(now);
    asked.map(left).min().unwrap_or(sweep)
}

fn ask(
    core: &SyncSender<core::Command>,
    session: &Addressed,
    troubles: &mut Troubles,
) -> Option<()> {
    let Addressed { id, name, .. } = session;
    let asked = core::Asked { id: *id, name: name.clone() };
    let said = |why: &String| troubles.say(format!("the panes of {name}: {why}"));
    if let Ok(panes) = list(name).inspect_err(said) {
        core.send(core::Command::Panes { asked: asked.clone(), panes }).ok()?;
    }
    let said = |why: &Failure| troubles.say(format!("the clients of {name}: {why}"));
    if let Ok(clients) = Mux::Zellij(name).clients().inspect_err(said) {
        for client in clients.set_aside() {
            let (client, pane) = (client.id, client.pane);
            troubles.say(format!("the clients of {name}: set aside client {client} on {pane}"));
        }
        core.send(core::Command::Clients { asked, clients }).ok()?;
    }
    Some(())
}
