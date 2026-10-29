//! The panes of each zellij session that runs, as `zellij action list-panes`
//! lists them, and its clients.
//!
//! zellij tells no one as a pane opens, closes, or changes where it works or
//! what it runs, nor as a client comes, goes, or moves its focus. It writes
//! what it keeps of a session within a second of most of those, though, but
//! for where a pane works and what it runs, which luneta's hook has a shell
//! say as it runs a command. So a session is asked about again as either
//! changes, and every sweep besides, for the rest; and one the daemon has not
//! asked about yet as soon as it turns up.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use notify::RecursiveMode;
use serde::Deserialize;

use crate::api;
use crate::clients;
use crate::core::{self, Addressed};
use crate::metadata::{self, Metadata};
use crate::program;
use crate::shells::Shell;
use crate::watch::{self, Troubles};
use crate::zellij::Id;

/// How long zellij has to list a session's panes: a server that runs answers
/// at once.
const LIMIT: Duration = Duration::from_secs(2);

/// How long the daemon leaves a session's server be once it made its socket,
/// before it asks zellij anything: every call of zellij's CLI asks the server
/// of every session whether it runs, and zellij-server 0.45.1 panics where
/// such a call hangs up before the server took its first client (`lib.rs:1462`,
/// fixed after 0.45.1 in zellij's 81f56e1), and the session ends with it. The
/// client that started the server comes a few milliseconds after the socket,
/// and the socket wakes the sweep as soon as `watch` lets the folder settle.
const NEWBORN: Duration = Duration::from_millis(500);

/// A terminal of a session's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: u32,
    /// The position of its tab, from 0.
    pub tab: u32,
    /// The id zellij gave its tab, which stays the tab's as it moves and as
    /// tabs before it close.
    pub tab_id: u32,
    pub tab_name: String,
    pub cwd: Option<Said>,
    /// What runs in front in it.
    pub command: Option<Said>,
    /// What an agent says in its title, as `titled` has it.
    pub titled: Option<Titled>,
}

impl Pane {
    /// The pane as the API gives it, with what its `shell` says.
    #[must_use]
    pub fn to_api(&self, shell: Option<&Shell>) -> api::Pane {
        api::Pane {
            id: self.id,
            tab: self.tab,
            tab_name: self.tab_name.clone(),
            cwd: self.cwd.clone().map(String::from),
            command: self.command.clone().map(String::from),
            titled: self.titled.as_ref().map(Titled::to_api),
            shell: shell.map(Shell::to_api),
            tab_id: Some(self.tab_id),
        }
    }
}

/// What an agent says in the title of its pane, without the glyph it leads
/// the title with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Titled {
    /// Its topic: none while Claude Code titles the pane a bare `Claude Code`,
    /// as it does until it has one.
    pub topic: Option<Said>,
}

impl Titled {
    fn to_api(&self) -> api::Titled {
        api::Titled { topic: self.topic.clone().map(String::from) }
    }
}

/// A pane as zellij 0.45 lists it, as much of it as the daemon reads. zellij
/// leaves out where a pane works and what runs in it when it does not know,
/// and writes the rest every time: a listing without one does not read, where
/// a default would have a plugin's pane read as a terminal in the first tab.
#[derive(Deserialize)]
struct Wire {
    id: u32,
    is_plugin: bool,
    is_suppressed: bool,
    tab_id: u32,
    tab_position: u32,
    tab_name: String,
    #[serde(default)]
    pane_cwd: Option<String>,
    #[serde(default)]
    pane_command: Option<String>,
    title: String,
}

/// The terminals in what zellij listed, or why it does not read. A plugin's
/// pane is no terminal, and a suppressed one is off the screen, in the place
/// another took. zellij gives each terminal of a session an id of its own.
fn parse(said: &[u8]) -> Result<Vec<Pane>, String> {
    let wire: Vec<Wire> = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let panes: Vec<Pane> = wire
        .into_iter()
        .filter(|pane| !pane.is_plugin && !pane.is_suppressed)
        .map(|pane| Pane {
            id: pane.id,
            tab: pane.tab_position,
            tab_id: pane.tab_id,
            tab_name: pane.tab_name,
            cwd: pane.pane_cwd.and_then(Said::new),
            command: pane.pane_command.and_then(Said::new),
            titled: titled(&pane.title),
        })
        .collect();
    let mut ids = HashSet::new();
    if let Some(twice) = panes.iter().find(|pane| !ids.insert(pane.id)) {
        return Err(format!("pane {} comes twice", twice.id));
    }
    Ok(panes)
}

/// The title Claude Code gives its pane until the agent has a topic, which
/// some never get.
const GENERIC_TITLE: &str = "Claude Code";

/// What an agent says in a pane's title, without the glyph it leads the title
/// with: Claude Code's `✳` at rest, and the frame it turns in its place while
/// it works, `◐` and `◑` as 2.1.310 turns them, so that the host changes only
/// as the agent's topic does. None for a title that leads with no such glyph,
/// which no agent gave.
fn titled(title: &str) -> Option<Titled> {
    let mut chars = title.trim().chars();
    let glyph = chars.next()?;
    let agent = matches!(glyph, '✳' | '◐' | '◑' | '✢' | '✶' | '✻' | '✽' | '·')
        || ('\u{2800}'..='\u{28ff}').contains(&glyph);
    let topic = Some(chars.as_str().trim_start()).filter(|topic| *topic != GENERIC_TITLE);
    agent.then(|| Titled { topic: topic.map(str::to_owned).and_then(Said::new) })
}

/// Keeps in `panes` where each worked and what ran in it as `listed` said,
/// where zellij says nothing of either now. It gives itself 100ms to find
/// out each for each pane, and says nothing when it takes longer, as a
/// server that is busy does (`route.rs` in zellij-server 0.45.1).
pub fn keep(panes: &mut [Pane], listed: &[Pane]) {
    for pane in panes {
        let Some(was) = listed.iter().find(|was| was.id == pane.id) else {
            continue;
        };
        pane.cwd = pane.cwd.take().or_else(|| was.cwd.clone());
        pane.command = pane.command.take().or_else(|| was.command.clone());
    }
}

/// The panes of `session`, or why zellij did not list them.
fn list(session: &str) -> Result<Vec<Pane>, String> {
    let mut zellij = Command::new("zellij");
    zellij.args(["--session", session, "action", "list-panes", "--all", "--json"]);
    let ran = program::run(&mut zellij, LIMIT)
        .map_err(|error| format!("zellij could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    parse(&said).map_err(|why| format!("zellij listed what does not read: {why}"))
}

/// What tells the daemon that the panes of a session may have changed, short
/// of asking zellij.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Signs {
    /// What zellij last wrote of it.
    metadata: Option<Metadata>,
    /// The last word said of it, as the core counts them.
    told: u64,
}

/// When the daemon last asked zellij about a session, and its signs then.
struct Asked {
    /// When it was done asking, rather than when it began: the daemon gives
    /// zellij up to `LIMIT` for a session's panes and as long for its
    /// clients, before it gives up on a server that hangs, longer than a
    /// sweep. Due again as soon as it was given up on, a session never listed
    /// would go first in every round a wake begins anew, and no other would
    /// be asked about while the wakes kept coming.
    at: Instant,
    signs: Signs,
}

/// Asks zellij for the panes and the clients of each session it can be asked
/// about, in a thread of its own, and tells the core what it says. It asks
/// about a session as soon as it turns up, as zellij writes in
/// `metadata_folder` that it changed, as a shell in it says a word, and once
/// a `sweep` has passed since it last asked, for what changes with no sign: a
/// shell without luneta's hook moves or runs a command, or an agent retitles
/// its pane. The core wakes it on `woken`, and so does a change to the folder,
/// which it watches with `wake`. One the host has no panes of goes first, and
/// one that turns up while others are asked about goes before those left.
/// None is asked about while a session is `NEWBORN`. The thread ends once the
/// core has.
pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    metadata_folder: Option<PathBuf>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("panes".to_owned()).spawn(move || {
        let mut asked: HashMap<Id, Asked> = HashMap::new();
        let mut troubles: HashMap<Id, Troubles> = HashMap::new();
        let mut watcher = None;
        let mut watching = Troubles::default();
        'round: loop {
            if let Some(folder) = metadata_folder.as_deref()
                && watcher.is_none()
            {
                let (mode, wanted) = (RecursiveMode::Recursive, metadata::is_metadata);
                watcher = watch::watching(folder, mode, wanted, &wake, &mut watching);
                watching.done();
            }
            let Some(sessions) = core::ask(&core, core::Command::Sessions) else {
                return;
            };
            let started = sessions.iter().map(|session| session.id.started());
            if let Some(wait) = newborn(started, SystemTime::now()) {
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
                        .as_deref()
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

/// What zellij last wrote of the session `name` in `folder`. One that cannot
/// be read, `troubles` says, and it says nothing.
fn written(folder: &Path, name: &str, troubles: &mut Troubles) -> Option<Metadata> {
    metadata::read(folder, name).unwrap_or_else(|error| {
        troubles.say(format!("the metadata of {name}: {error}"));
        None
    })
}

/// Whether a session with `signs` is due to be asked about at `now`, as it
/// was `asked` last: one never asked about, one whose signs have changed
/// since, and one last asked about a `sweep` ago.
fn due(asked: Option<&Asked>, signs: Signs, now: Instant, sweep: Duration) -> bool {
    asked.is_none_or(|asked| {
        asked.signs != signs || now.saturating_duration_since(asked.at) >= sweep
    })
}

/// How long until the first of the sessions `asked` is due for the sweep, at
/// `now`: a `sweep`, of none.
fn next<'a>(asked: impl Iterator<Item = &'a Asked>, sweep: Duration, now: Instant) -> Duration {
    let left = |asked: &Asked| (asked.at + sweep).saturating_duration_since(now);
    asked.map(left).min().unwrap_or(sweep)
}

/// Asks zellij for the panes and the clients of `session`, and tells the core
/// what it says: what zellij listed last stands until it lists them again.
/// None once the core has stopped.
fn ask(
    core: &SyncSender<core::Command>,
    session: &Addressed,
    troubles: &mut Troubles,
) -> Option<()> {
    let Addressed { id, name, .. } = session;
    let said = |why: &String| troubles.say(format!("the panes of {name}: {why}"));
    if let Ok(panes) = list(name).inspect_err(said) {
        core.send(core::Command::Panes { session: *id, name: name.clone(), panes }).ok()?;
    }
    let said = |why: &String| troubles.say(format!("the clients of {name}: {why}"));
    if let Ok(clients) = clients::list(Command::new("zellij"), name).inspect_err(said) {
        core.send(core::Command::Clients { session: *id, name: name.clone(), clients }).ok()?;
    }
    Some(())
}

/// How long until the youngest of the sessions that `started` so is no longer
/// `NEWBORN`, at `now`, where one is. One that started after `now`, by a clock
/// set back since, is taken for old: it would hold the sweep up for as long
/// as the clock went back.
fn newborn(started: impl Iterator<Item = SystemTime>, now: SystemTime) -> Option<Duration> {
    let left = |started| NEWBORN.checked_sub(now.duration_since(started).ok()?);
    started.filter_map(left).filter(|left| !left.is_zero()).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep waits out the youngest session that is newborn, and none
    /// that is not. One that started after now, as by a clock set back since,
    /// holds nothing up: the sweep would wait for as long as the clock went
    /// back.
    #[test]
    fn the_sweep_waits_out_the_youngest_newborn_session() {
        let now = SystemTime::now();
        let ago = |age: Duration| now - age;
        let quarter = NEWBORN / 4;
        let started = [ago(quarter), ago(quarter * 3), ago(NEWBORN * 4)];
        assert_eq!(newborn(started.into_iter(), now), Some(quarter * 3));
        assert_eq!(newborn([ago(NEWBORN), ago(NEWBORN * 4)].into_iter(), now), None);
        assert_eq!(newborn([now + Duration::from_mins(1)].into_iter(), now), None);
        assert_eq!(newborn(std::iter::empty(), now), None);
    }
}
