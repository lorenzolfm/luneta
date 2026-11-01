//! The other machines the daemon watches, as luneta's hosts file names them:
//! `hosts` in luneta's state folder, an ssh destination to a line, which the
//! daemon reads again as it changes. Each machine is watched from a thread of
//! its own, through `ssh <destination> luneta bridge`, as a luneta watches the
//! daemon on its socket, and what its daemon says comes in the host this one
//! says. A thread whose machine leaves the file ends, and its ssh with it.
//!
//! A machine's host comes as a `Remote.Host`, which has neither remotes of
//! its own nor a version. Two daemons that each watch the other would
//! otherwise change their hosts for as long as both ran: each change to one
//! is a change to the other's remotes, and so to its version, which is a
//! change to the first one's.

use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::iter;
use std::path::Path;
use std::process::Command as Process;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, SystemTime};

use notify::RecursiveMode;
use tokio::sync::oneshot;
use tonic::transport::Channel;

use crate::api::daemon_client::DaemonClient;
use crate::api::remote::{self, unreached};
use crate::api;
use crate::client::{self, Unbridged};
use crate::core::Command;
use crate::log::{self, causes};
use crate::state;
use crate::watch::{self, Troubles};

/// The file, in luneta's state folder.
const FILE: &str = "hosts";

/// The most machines the daemon watches besides its own, a thread and an ssh
/// to each. The lines past these are left out, and the log says so.
const HOSTS_MAX: usize = 16;

/// The longest the daemon waits before it runs ssh again to a machine it
/// failed to reach: short enough that a machine that wakes, or a link that
/// comes back, shows soon, and long enough that one that is gone for the day
/// costs an ssh, and a line in the log, only now and then.
const AGAIN_MAX: Duration = Duration::from_secs(30);

/// A machine, as ssh is told it: a line of the file, without the white space
/// around it. One with white space or a control character in it is no
/// destination, as ssh would take it for more than one word, and nor is one
/// led by a `-`, which ssh would take for an option.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Destination(String);

impl Destination {
    /// The destination of `line`, none for a blank one, or why it is none.
    fn parse(line: &str) -> Result<Option<Destination>, String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        if line.starts_with('-') {
            return Err(format!("{line:?} is led by a '-', which ssh would take for an option"));
        }
        if line.chars().any(|char| char.is_whitespace() || char.is_control()) {
            return Err(format!("{line:?} is more than one word"));
        }
        Ok(Some(Destination(line.to_owned())))
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the daemon has of a machine it watches.
pub enum Reached {
    /// Nothing yet: the first ssh to it runs.
    Reaching,
    /// Its host, as its daemon last said it.
    Host(remote::Host),
    /// Nothing, since its daemon last answered, or since the watch began.
    Unreached { since: SystemTime, why: String, stopped: Stopped },
}

impl Reached {
    pub fn to_api(&self, destination: &Destination) -> api::Remote {
        let reached = match self {
            Reached::Reaching => remote::Reached::Reaching(remote::Reaching {}),
            Reached::Host(host) => remote::Reached::Host(host.clone()),
            Reached::Unreached { since, why, stopped } => {
                remote::Reached::Unreached(remote::Unreached {
                    since: Some((*since).into()),
                    why: why.clone(),
                    stopped: Some(stopped.to_api()),
                })
            },
        };
        api::Remote { destination: destination.0.clone(), reached: Some(reached) }
    }
}

/// What stopped the daemon from reaching a machine, in a word, as a client
/// would say what to do of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// ssh did not start, or what it said could not be read.
    Unstarted,
    /// ssh ended before it reached lunetad there, with its exit code: none
    /// where a signal ended it.
    Ended(Option<i32>),
    /// Something there said what is not the bridge's word first.
    Said,
    /// ssh neither reached lunetad there nor ended within `client::REACH`.
    Silent,
    /// ssh reached lunetad there, and what came after did not do.
    Lost,
}

impl Stopped {
    /// What stopped the daemon, as the `Unbridged` among the sources of
    /// `error` says: where there is none, ssh reached lunetad.
    fn of(error: &(dyn Error + 'static)) -> Stopped {
        let mut sources = iter::successors(Some(error), |&error| error.source());
        match sources.find_map(|error| error.downcast_ref::<Unbridged>()) {
            Some(Unbridged::Io(_)) => Stopped::Unstarted,
            Some(Unbridged::Ended(ended)) => Stopped::Ended(ended.code()),
            Some(Unbridged::Said(_)) => Stopped::Said,
            Some(Unbridged::Silent) => Stopped::Silent,
            None => Stopped::Lost,
        }
    }

    fn to_api(self) -> unreached::Stopped {
        match self {
            Stopped::Unstarted => unreached::Stopped::Unstarted(unreached::Unstarted {}),
            Stopped::Ended(code) => unreached::Stopped::Ended(unreached::Ended { code }),
            Stopped::Said => unreached::Stopped::Said(unreached::Said {}),
            Stopped::Silent => unreached::Stopped::Silent(unreached::Silent {}),
            Stopped::Lost => unreached::Stopped::Lost(unreached::Lost {}),
        }
    }
}

/// Why `error` leaves a machine unreached: in words, and in a word.
fn unreached(error: &(dyn Error + 'static)) -> (String, Stopped) {
    (causes(error), Stopped::of(error))
}

/// Watches each machine the file names, from a thread of its own, and tells
/// the core what its daemon says, or why it says nothing. The file is read
/// once before this returns, and the core told which machines it names: the
/// daemon's first answer has each, as one it is reaching. Then it is read
/// again as it changes, and at least every `sweep`.
pub fn follow(core: SyncSender<Command>, sweep: Duration) -> io::Result<()> {
    let file = match state::path(FILE) {
        Ok(file) => file,
        Err(unset) => {
            log::say(&format!("no other machine is watched: {unset}"));
            return Ok(());
        },
    };
    let dir = file.parent().expect("a file in luneta's state folder").to_owned();
    let mut troubles = Troubles::default();
    let read = move || {
        let named = named(&file, &mut troubles);
        troubles.done();
        named
    };
    // The machines the core was told of last, in the file's order, and what
    // ends the watch of each that is watched.
    let mut told: Vec<Destination> = Vec::new();
    let mut watched: Vec<(Destination, oneshot::Sender<()>)> = Vec::new();
    let tell = move |named: Option<Vec<Destination>>| {
        // A file that does not read leaves the machines as they were.
        let Some(named) = named else {
            return true;
        };
        if named == told {
            return true;
        }
        if core.send(Command::Hosts(named.clone())).is_err() {
            return false;
        }
        // The core no longer takes in what the thread of one that went says.
        watched.retain(|(destination, _)| named.contains(destination));
        for destination in &named {
            if watched.iter().any(|(watched, _)| watched == destination) {
                continue;
            }
            match watch(destination.clone(), core.clone()) {
                Ok(ends) => watched.push((destination.clone(), ends)),
                // The core has it as one it is reaching, until the file
                // changes.
                Err(error) => log::say(&format!("{destination} is not watched: {error}")),
            }
        }
        told = named;
        true
    };
    let wanted = |path: &Path| path.file_name() == Some(OsStr::new(FILE));
    watch::watch("hosts", dir, RecursiveMode::NonRecursive, wanted, sweep, read, tell)
}

/// The machines the file at `path` names, in its order, each once and
/// `HOSTS_MAX` at the most: none where there is no file. None for a file that
/// does not read. What is wrong with it, `troubles` says.
fn named(path: &Path, troubles: &mut Troubles) -> Option<Vec<Destination>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Some(Vec::new()),
        Err(error) => {
            troubles.say(format!("{}: {error}", path.display()));
            return None;
        },
    };
    let mut seen = HashSet::new();
    let mut named = Vec::new();
    for line in text.lines() {
        match Destination::parse(line) {
            Ok(Some(destination)) if seen.insert(destination.clone()) => named.push(destination),
            Ok(_) => {},
            Err(why) => troubles.say(format!("{}: {why}, and is left out", path.display())),
        }
    }
    if named.len() > HOSTS_MAX {
        let past: Vec<String> = named.drain(HOSTS_MAX..).map(|it| it.0).collect();
        let (file, max, past) = (path.display(), HOSTS_MAX, past.join(", "));
        troubles.say(format!("{file} names more than {max}: {past} are left out"));
    }
    Some(named)
}

/// Watches `destination` from a thread of its own, until what this returns
/// is dropped.
fn watch(destination: Destination, core: SyncSender<Command>) -> io::Result<oneshot::Sender<()>> {
    let (ends, ending) = oneshot::channel();
    thread::Builder::new().name(format!("remote {destination}")).spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                log::say(&format!("{destination} is not watched: no runtime: {error}"));
                return;
            },
        };
        // What runs on the runtime goes with it as the thread ends: the
        // connection to the machine's daemon, and its ssh, killed as it goes.
        runtime.block_on(async move {
            tokio::spawn(watched(destination, core));
            let _ = ending.await;
        });
    })?;
    Ok(ends)
}

/// Asks the daemon of `destination` for its host again each time it answers,
/// and tells the core each that changed, or why it has none, until the core
/// has stopped. A connection that breaks is made afresh at once, and the host
/// last said stands meanwhile, as where the machine's daemon makes way for a
/// newer one. Where that fails too, the core is told that the machine is
/// unreached, and ssh is run again after a while, longer each time it fails.
async fn watched(destination: Destination, core: SyncSender<Command>) {
    let mut daemon = None;
    // The host as last told, as its daemon versions it.
    let mut after = None;
    let mut word = SystemTime::now();
    let mut failures: u32 = 0;
    loop {
        let (version, host) = match asked(&destination, &mut daemon, after).await {
            Ok(answered) => answered,
            Err((why, stopped)) => {
                failures = failures.saturating_add(1);
                if failures > 1 || after.is_none() {
                    // Asked afresh once reached again: the host as last told
                    // is no longer what the core has.
                    after = None;
                    let unreached = Reached::Unreached { since: word, why, stopped };
                    if !told(&core, &destination, unreached) {
                        return;
                    }
                }
                tokio::time::sleep(again(failures)).await;
                continue;
            },
        };
        failures = 0;
        word = SystemTime::now();
        if after == Some(version) {
            continue;
        }
        after = Some(version);
        // Each field by name, and no `..`: a field the host gains does not
        // build here until it is said whether a machine's host has it too,
        // which one that changes with each daemon must not.
        let api::Host {
            version: _,
            sessions,
            work_trees,
            agents,
            unread,
            pull_requests,
            unread_sockets,
            unread_files,
            remotes: _,
        } = host;
        let host = remote::Host {
            sessions,
            work_trees,
            agents,
            unread,
            pull_requests,
            unread_sockets,
            unread_files,
        };
        if !told(&core, &destination, Reached::Host(host)) {
            return;
        }
    }
}

/// Tells the core what `reached` says of `destination`, and whether it still
/// listens.
fn told(core: &SyncSender<Command>, destination: &Destination, reached: Reached) -> bool {
    core.send(Command::Remote { destination: destination.clone(), reached }).is_ok()
}

/// What the daemon of `destination` answers `Watch` after `after`, and its
/// version, through the client in `daemon`, made first where there is none,
/// or why it answers none. One that failed a question is kept: it makes a
/// connection afresh for the next.
async fn asked(
    destination: &Destination,
    daemon: &mut Option<DaemonClient<Channel>>,
    after: Option<api::Version>,
) -> Result<(api::Version, api::Host), (String, Stopped)> {
    let daemon = if let Some(daemon) = daemon {
        daemon
    } else {
        let reached = client::through(ssh(destination)).await;
        daemon.insert(reached.map_err(|error| unreached(&error))?)
    };
    let watched = daemon.watch(api::WatchRequest { after }).await;
    let host = watched.map_err(|status| unreached(&status))?.into_inner();
    // One with no version would be asked after again at once, as a first
    // look, for as long as it answers so.
    let unversioned = "an answer with no version, which this lunetad does not read";
    let version = host.version.ok_or_else(|| (unversioned.to_owned(), Stopped::Lost))?;
    Ok((version, host))
}

/// What runs `luneta bridge` on `destination`, made afresh for each
/// connection: with no terminal, which would mangle what the daemons say, and
/// no prompt, which no one would see.
fn ssh(destination: &Destination) -> impl FnMut() -> Process + Send + 'static {
    let destination = destination.0.clone();
    move || {
        let mut ssh = Process::new("ssh");
        ssh.args(["-T", "-o", "BatchMode=yes", &destination, "luneta", "bridge"]);
        ssh
    }
}

/// How long the daemon waits to run ssh again to a machine after `failures`
/// in a row: not at all after the first, as for a connection that broke, then
/// a second, and twice as long after each failure after that, up to
/// `AGAIN_MAX`.
fn again(failures: u32) -> Duration {
    let Some(doubled) = failures.checked_sub(2) else {
        return Duration::ZERO;
    };
    let secs = 1_u64.checked_shl(doubled).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(AGAIN_MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn named_in(text: &str) -> Vec<String> {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, text).unwrap();
        let named = named(&file, &mut Troubles::default()).unwrap();
        named.iter().map(ToString::to_string).collect()
    }

    /// A line is a machine, without the white space around it, in the file's
    /// order and once: a blank line is none, and so is one ssh would take
    /// for an option, or for more than one word.
    #[test]
    fn a_line_names_a_machine_once() {
        let text = "nixos\n\n  me@vps \t\nnixos\n-oProxyCommand=sh\nmy vps\nssh://me@vps:22\n";
        assert_eq!(named_in(text), ["nixos", "me@vps", "ssh://me@vps:22"]);
    }

    /// No file names no machine, and a file names `HOSTS_MAX` at the most.
    #[test]
    fn no_file_names_none_and_a_file_names_a_few_at_the_most() {
        let scratch = tempfile::tempdir().unwrap();
        let none = named(&scratch.path().join(FILE), &mut Troubles::default());
        assert_eq!(none, Some(Vec::new()));
        let many: Vec<String> = (0..HOSTS_MAX + 3).map(|n| format!("host-{n}")).collect();
        let named = named_in(&many.join("\n"));
        assert_eq!(named.len(), HOSTS_MAX);
        assert_eq!(named.last().unwrap(), &format!("host-{}", HOSTS_MAX - 1));
    }

    /// ssh runs again at once after a first failure, then after a second,
    /// and twice as long each time, up to half a minute.
    #[test]
    fn ssh_runs_again_after_longer_each_time_it_fails() {
        let secs: Vec<u64> = (1..=9).map(|failures| again(failures).as_secs()).collect();
        assert_eq!(secs, [0, 1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(again(u32::MAX), Duration::from_secs(30));
    }
}
