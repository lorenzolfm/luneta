//! The panes of each zellij session that runs, as `zellij action list-panes`
//! lists them, and its clients.
//!
//! zellij tells no one as a pane opens, closes, or changes where it works or
//! what it runs, nor as a client comes, goes, or moves its focus. So each
//! session is asked again every sweep, and one the daemon has not asked about
//! yet as soon as it turns up.

use std::collections::HashSet;
use std::io;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Duration;

use luneta_protocol::said::Said;
use serde::Deserialize;

use crate::api;
use crate::clients;
use crate::core;
use crate::program;
use crate::watch::Troubles;
use crate::zellij::Id;

/// How long zellij has to list a session's panes: a server that runs answers
/// at once.
const LIMIT: Duration = Duration::from_secs(2);

/// A terminal of a session's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: u32,
    /// The position of its tab, from 0.
    pub tab: u32,
    pub tab_name: String,
    pub cwd: Option<Said>,
    /// What runs in front in it.
    pub command: Option<Said>,
    /// What an agent says in its title, as `titled` has it.
    pub titled: Option<Titled>,
}

impl Pane {
    #[must_use]
    pub fn to_api(&self) -> api::Pane {
        api::Pane {
            id: self.id,
            tab: self.tab,
            tab_name: self.tab_name.clone(),
            cwd: self.cwd.clone().map(String::from),
            command: self.command.clone().map(String::from),
            titled: self.titled.as_ref().map(Titled::to_api),
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
/// it works, so that the host changes only as the agent's topic does. None for
/// a title that leads with no such glyph, which no agent gave.
fn titled(title: &str) -> Option<Titled> {
    let mut chars = title.trim().chars();
    let glyph = chars.next()?;
    let agent = matches!(glyph, '✳' | '✢' | '✶' | '✻' | '✽' | '·')
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

/// Asks zellij for the panes and the clients of each session it can be asked
/// about, in a thread of its own, and tells the core what it says: every
/// `sweep`, and at once when `woken`, as a session turns up. One zellij has
/// not listed the panes of yet goes first, and one that turns up while the
/// others are asked about goes before those left. The thread ends once the
/// core has.
pub fn follow(
    core: SyncSender<core::Command>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("panes".to_owned()).spawn(move || {
        let mut listed: HashSet<Id> = HashSet::new();
        let mut troubles = Troubles::default();
        'sweep: loop {
            let Some(mut sessions) = core::ask(&core, core::Command::Sessions) else {
                return;
            };
            listed.retain(|session| sessions.iter().any(|(id, _)| id == session));
            sessions.sort_by_key(|(session, _)| listed.contains(session));
            for (session, name) in sessions {
                // What zellij listed last stands until it lists them again.
                let said = |why: &String| troubles.say(format!("the panes of {name}: {why}"));
                if let Ok(panes) = list(&name).inspect_err(said) {
                    listed.insert(session);
                    let name = name.clone();
                    if core.send(core::Command::Panes { session, name, panes }).is_err() {
                        return;
                    }
                }
                let said = |why: &String| troubles.say(format!("the clients of {name}: {why}"));
                let zellij = Command::new("zellij");
                if let Ok(clients) = clients::list(zellij, &name).inspect_err(said)
                    && core.send(core::Command::Clients { session, name, clients }).is_err()
                {
                    return;
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            troubles.done();
            match woken.recv_timeout(sweep) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}
