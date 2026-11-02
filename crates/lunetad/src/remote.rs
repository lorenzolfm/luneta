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
//! change to the first one's. And each machine's daemon is asked only while
//! this one is kept, runs for reasons of its own, which the question says,
//! and which only then keeps that one running too: two that each watch the
//! other would else keep each other running for good, or start each other
//! again through the bridge as each ended.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, BufRead as _, BufReader, ErrorKind, PipeWriter, Read as _};
use std::iter;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::str::FromStr;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use notify::RecursiveMode;
use tokio::sync::oneshot;
use tokio::sync::watch::Receiver;
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
pub const FILE: &str = "hosts";

/// The most machines the daemon watches besides its own, a thread and an ssh
/// to each. The lines past these are left out, and the log says so.
pub const HOSTS_MAX: usize = 16;

/// The longest the daemon waits before it runs ssh again to a machine it
/// failed to reach: short enough that a machine that wakes, or a link that
/// comes back, shows soon, and long enough that one that is gone for the day
/// costs an ssh, and a line in the log, only now and then.
const AGAIN_MAX: Duration = Duration::from_secs(30);

/// The most of a line ssh says on its stderr that the log keeps, in bytes:
/// what comes after it is said as a line of its own.
const SAID_MAX: u64 = 4096;

/// How long what an ssh says on its stderr waits to be heard for what the
/// ssh before it said, so as not to say it again.
const HEARD_BEFORE: Duration = Duration::from_secs(1);

/// For how long, in seconds, the connection every ssh to a machine shares
/// stays up once the last of them ended, for the next to take up.
const SHARED_FOR: u32 = 60;

/// How often, in seconds, the shared connection asks the machine whether it
/// is there while it hears nothing, and how many it goes without an answer
/// before it ends: one that stalls ends in this long, as a link does that
/// went down, and the next ssh makes another.
const ALIVE_EVERY: u32 = 5;
const ALIVE_MISSES: u32 = 2;

/// The longest path of a Unix socket, in bytes, on macOS, whose room for one
/// is shorter than Linux's 108, less its terminating byte.
const SOCKET_PATH_MAX: usize = 103;

/// How much ssh adds to the path of the shared connection's socket as it
/// makes it: a dot and 16 characters, which it then renames away.
const SOCKET_MAKING: usize = 17;

/// A machine, as ssh is told it: a line of the file, without the white space
/// around it. One with white space or a control character in it is no
/// destination, as ssh would take it for more than one word, and nor is one
/// led by a `-`, which ssh would take for an option. One spelled with `parse`
/// is one the daemon reads from the file, whoever wrote it there.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Destination(String);

impl FromStr for Destination {
    type Err = String;

    /// The destination `text` spells, without the white space around it, or
    /// why it spells none.
    fn from_str(text: &str) -> Result<Destination, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("a destination is not blank".to_owned());
        }
        if text.starts_with('-') {
            return Err(format!("{text:?} is led by a '-', which ssh would take for an option"));
        }
        if text.chars().any(|char| char.is_whitespace() || char.is_control()) {
            return Err(format!("{text:?} is more than one word"));
        }
        Ok(Destination(text.to_owned()))
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

/// The daemons of the machines this one watches, each through the bridge it
/// watches it on, while the last question there was answered: for a
/// client's question of one, which this one hands on. Each with the number
/// of the watch that reached it.
#[derive(Default)]
pub struct Bridges(Mutex<HashMap<Destination, (u64, DaemonClient<Channel>)>>);

impl Bridges {
    /// The daemon of `destination`, while this one reaches it.
    pub fn to(&self, destination: &Destination) -> Option<DaemonClient<Channel>> {
        locked(&self.0).get(destination).map(|(_, daemon)| daemon.clone())
    }
}

/// `destination`'s daemon in `bridges`, from its watch's first answer to the
/// last it had, or until the watch stops.
struct Bridged {
    bridges: Arc<Bridges>,
    destination: Destination,
    /// The watch's number, of those `follow` started, each after the one
    /// before. A machine forgotten and added again has a watch that ends as
    /// the next begins: the one that ends neither takes out nor writes over
    /// what the later one has there.
    watch: u64,
}

impl Bridged {
    fn reached(&self, daemon: &DaemonClient<Channel>) {
        let mut bridges = locked(&self.bridges.0);
        let later = bridges.get(&self.destination).is_some_and(|(watch, _)| *watch > self.watch);
        if !later {
            bridges.insert(self.destination.clone(), (self.watch, daemon.clone()));
        }
    }

    fn lost(&self) {
        let mut bridges = locked(&self.bridges.0);
        if bridges.get(&self.destination).is_some_and(|(watch, _)| *watch == self.watch) {
            bridges.remove(&self.destination);
        }
    }
}

impl Drop for Bridged {
    fn drop(&mut self) {
        self.lost();
    }
}

/// The options that have an ssh to `destination` share one connection with
/// every other that luneta and its daemon run to it, on a socket in luneta's
/// runtime `folder`: the first to run makes it, and leaves it to run in the
/// background for as long as any uses it, and `SHARED_FOR` after. So a
/// client of zellij's that luneta runs there through ssh comes up with no
/// login of its own, and what luneta has the daemon there do goes after the
/// keys it typed, on the one connection. One that stalls ends, as
/// `ALIVE_EVERY` says, for the next ssh to make another.
///
/// The socket is named for the destination, short, as the path of a Unix
/// socket must be, and as ssh adds to it as it makes one: macOS's `TMPDIR`,
/// where the folder is there, leaves no room for ssh's own `%C`. A folder
/// that leaves no room either has none: each ssh then has a connection of
/// its own, as ssh with a socket's path too long for it would not run.
#[must_use]
pub fn sharing(folder: &Path, destination: &Destination) -> Vec<OsString> {
    // FNV-1a, which names it alike in every build of luneta and its daemon.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in destination.0.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name = format!("/ssh-{hash:016x}");
    let folder = folder.as_os_str().as_bytes();
    if folder.len() + name.len() + SOCKET_MAKING > SOCKET_PATH_MAX {
        return Vec::new();
    }
    // ssh reads a `%` in the path as one of its own tokens, and `%%` as `%`.
    let mut path = Vec::new();
    for &byte in folder {
        path.extend_from_slice(if byte == b'%' { b"%%" } else { std::slice::from_ref(&byte) });
    }
    path.extend_from_slice(name.as_bytes());
    let mut control = OsString::from("ControlPath=");
    control.push(OsStr::from_bytes(&path));
    let options = [
        OsString::from("ControlMaster=auto"),
        control,
        OsString::from(format!("ControlPersist={SHARED_FOR}")),
        OsString::from(format!("ServerAliveInterval={ALIVE_EVERY}")),
        OsString::from(format!("ServerAliveCountMax={ALIVE_MISSES}")),
    ];
    options.into_iter().flat_map(|option| [OsString::from("-o"), option]).collect()
}

/// What the hosts file names, as it was last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// The machines, in its order, each once, and `HOSTS_MAX` at the most.
    pub machines: Vec<Destination>,
    /// How much of it named no machine: each line that is no destination,
    /// each past the `HOSTS_MAX`th, and the file itself, where it did not
    /// read.
    pub unread: u32,
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
    /// The daemon could not start a thread to watch it from, nor a runtime.
    Unwatched,
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
            Stopped::Unwatched => unreached::Stopped::Unwatched(unreached::Unwatched {}),
        }
    }
}

/// Why `error` leaves a machine unreached: in words, and in a word.
fn unreached(error: &(dyn Error + 'static)) -> (String, Stopped) {
    (causes(error), Stopped::of(error))
}

/// Watches each machine the file names, from a thread of its own, and tells
/// the core what its daemon says, or why it says nothing, as `watched` asks
/// it, while `kept` says whether this daemon runs for reasons of its own. The
/// file is read once before this returns, and the core told which machines it
/// names: the daemon's first answer has each, as one it is reaching. Then it
/// is read again as it changes, and at least every `sweep`. Each machine's
/// daemon is in `bridges` while it is reached, and its ssh shares its
/// connection, as `sharing` says, on a socket in luneta's runtime `folder`.
pub fn follow(
    core: SyncSender<Command>,
    kept: Receiver<bool>,
    sweep: Duration,
    folder: PathBuf,
    bridges: Arc<Bridges>,
) -> io::Result<()> {
    let file = match state::path(FILE) {
        Ok(file) => file,
        Err(unset) => {
            log::say(&format!("no other machine is watched: {unset}"));
            // A file there is nowhere to look for is one that does not read.
            let _ = core.send(Command::Hosts(Named { machines: Vec::new(), unread: 1 }));
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
    // What the core was told the file names last, what ends the watch of
    // each machine that a thread watches, and since when each that none does
    // has gone without one.
    let mut told = Named::default();
    let mut watched: Vec<(Destination, oneshot::Sender<()>)> = Vec::new();
    let mut unwatched: HashMap<Destination, SystemTime> = HashMap::new();
    let mut unwatchable = Troubles::default();
    // How many watches it began, which numbers each.
    let mut begun: u64 = 0;
    let tell = move |named: Option<Named>| {
        // A file that does not read leaves the machines as they were.
        let named = match named {
            Some(named) => named,
            None => Named { machines: told.machines.clone(), unread: 1 },
        };
        if named != told {
            if core.send(Command::Hosts(named.clone())).is_err() {
                return false;
            }
            // The core no longer takes in what the thread of one that went
            // says.
            let machines = &named.machines;
            watched.retain(|(destination, _)| machines.contains(destination));
            unwatched.retain(|destination, _| machines.contains(destination));
            told = named;
        }
        // One whose watch did not start is unreached, and its watch is
        // started again on each read, until one does.
        for destination in &told.machines {
            if watched.iter().any(|(watched, _)| watched == destination) {
                continue;
            }
            let bridges = Arc::clone(&bridges);
            begun += 1;
            let daemon = Bridged { bridges, destination: destination.clone(), watch: begun };
            match watch(daemon, core.clone(), kept.clone(), folder.clone()) {
                Ok(ends) => {
                    unwatched.remove(destination);
                    watched.push((destination.clone(), ends));
                },
                Err(error) => {
                    unwatchable.say(format!("{destination} is not watched: {error}"));
                    let since = unwatched.entry(destination.clone());
                    let since = *since.or_insert_with(SystemTime::now);
                    let why = format!("it is not watched: {error}");
                    let reached = Reached::Unreached { since, why, stopped: Stopped::Unwatched };
                    let destination = destination.clone();
                    if core.send(Command::Remote { destination, reached }).is_err() {
                        return false;
                    }
                },
            }
        }
        unwatchable.done();
        true
    };
    let wanted = |path: &Path| path.file_name() == Some(OsStr::new(FILE));
    watch::watch("hosts", dir, RecursiveMode::NonRecursive, wanted, sweep, read, tell)
}

/// What the file at `path` names, as `named_by` reads it: no machine where
/// there is no file. None for a file that does not read. What is wrong with
/// it, `troubles` says.
fn named(path: &Path, troubles: &mut Troubles) -> Option<Named> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Some(Named::default()),
        Err(error) => {
            troubles.say(format!("{}: {error}", path.display()));
            return None;
        },
    };
    Some(named_by(&text, |what| troubles.say(format!("{}: {what}", path.display()))))
}

/// What `text`, as the hosts file has it, names: the machines, in its order,
/// each once, and `HOSTS_MAX` at the most. A blank line names none. A line
/// that is no destination, and a machine past the most, is left out, and
/// `trouble` told of it. luneta reads the file by this too, before it
/// writes it, so that it counts the machines as the daemon does.
pub fn named_by(text: &str, mut trouble: impl FnMut(String)) -> Named {
    let mut seen = HashSet::new();
    let mut named = Named::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match line.parse::<Destination>() {
            Ok(destination) if seen.insert(destination.clone()) => {
                named.machines.push(destination);
            },
            Ok(_) => {},
            Err(why) => {
                trouble(format!("{why}, and is left out"));
                named.unread = named.unread.saturating_add(1);
            },
        }
    }
    if named.machines.len() > HOSTS_MAX {
        let past: Vec<String> = named.machines.drain(HOSTS_MAX..).map(|it| it.0).collect();
        let many = u32::try_from(past.len()).unwrap_or(u32::MAX);
        named.unread = named.unread.saturating_add(many);
        let past = past.join(", ");
        trouble(format!("{past} are left out, as the daemon watches {HOSTS_MAX} at the most"));
    }
    named
}

/// Watches the machine `bridged` is of from a thread of its own, until what
/// this returns is dropped, once the thread has the runtime it watches on. It
/// makes that itself, and drops it: the first read is made on the daemon's
/// own runtime, where another one dropped would panic. Its ssh shares its
/// connection on a socket in `folder`.
fn watch(
    bridged: Bridged,
    core: SyncSender<Command>,
    kept: Receiver<bool>,
    folder: PathBuf,
) -> io::Result<oneshot::Sender<()>> {
    let (ends, ending) = oneshot::channel();
    let (built, building) = mpsc::sync_channel(1);
    let name = format!("remote {}", bridged.destination);
    thread::Builder::new().name(name).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = built.send(Err(error));
                return;
            },
        };
        let _ = built.send(Ok(()));
        // What runs on the runtime goes with it as the thread ends: the
        // connection to the machine's daemon, and its ssh, killed as it goes.
        runtime.block_on(async move {
            tokio::spawn(watched(bridged, core, kept, folder));
            let _ = ending.await;
        });
    })?;
    // One that ended before it said so panicked.
    let ended = |_| io::Error::other("its thread ended before it had a runtime");
    building.recv().map_err(ended)??;
    Ok(ends)
}

/// Asks the daemon of `destination` for its host again each time it answers,
/// and tells the core each that changed, or why it has none, until the core
/// has stopped. A connection that breaks is made afresh at once, and the host
/// last said stands meanwhile, as where the machine's daemon makes way for a
/// newer one. Where that fails too, the core is told that the machine is
/// unreached, and ssh is run again after a while, longer each time it fails.
/// It asks only while `kept` says that this daemon runs for reasons of its
/// own, which each question says, and which only then keeps that one running
/// too. The machine's daemon is in the bridges while its last question was
/// answered.
async fn watched(
    bridged: Bridged,
    core: SyncSender<Command>,
    mut kept: Receiver<bool>,
    folder: PathBuf,
) {
    let destination = bridged.destination.clone();
    let mut daemon = None;
    // The host as last told, as its daemon versions it.
    let mut after = None;
    let mut word = SystemTime::now();
    let mut failures: u32 = 0;
    let said = Arc::default();
    loop {
        // A question on a connection that broke makes one afresh, with an
        // ssh, which could start the daemon of a machine whose own had just
        // ended, through its `luneta bridge`. Two daemons that watched each
        // other, and that nothing else kept, would else start each other's
        // again, for good.
        if kept.wait_for(|kept| *kept).await.is_err() {
            // This daemon ends.
            return;
        }
        let is_kept = *kept.borrow();
        let asked = asked(&destination, &mut daemon, after, is_kept, &said, &folder).await;
        let (version, host) = match asked {
            Ok(answered) => answered,
            Err((why, stopped)) => {
                bridged.lost();
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
        if let Some(daemon) = &daemon {
            bridged.reached(daemon);
        }
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
            unread_hosts: _,
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
/// or why it answers none. It is told whether this daemon is `kept`. One that
/// failed a question is kept: it makes a connection afresh for the next. What
/// ssh says goes in the log as `heard` says it, with what it `said` before.
/// ssh shares its connection on a socket in `folder`.
async fn asked(
    destination: &Destination,
    daemon: &mut Option<DaemonClient<Channel>>,
    after: Option<api::Version>,
    kept: bool,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> Result<(api::Version, api::Host), (String, Stopped)> {
    let daemon = if let Some(daemon) = daemon {
        daemon
    } else {
        let reached = client::through(ssh(destination, said, folder)).await;
        daemon.insert(reached.map_err(|error| unreached(&error))?)
    };
    let watched = daemon.watch(api::WatchRequest { after, kept: Some(kept) }).await;
    let host = watched.map_err(|status| unreached(&status))?.into_inner();
    // One with no version would be asked after again at once, as a first
    // look, for as long as it answers so.
    let unversioned = "an answer with no version, which this lunetad does not read";
    let version = host.version.ok_or_else(|| (unversioned.to_owned(), Stopped::Lost))?;
    Ok((version, host))
}

/// What runs `luneta bridge` on `destination`, made afresh for each
/// connection: with no terminal, which would mangle what the daemons say, and
/// no prompt, which no one would see, on the connection it shares, as
/// `sharing` says, on a socket in `folder`. What each says on its stderr goes
/// in the log as `heard` says it, with what the one before it `said`.
fn ssh(
    destination: &Destination,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> impl FnMut() -> Process + Send + 'static {
    let sharing = sharing(folder, destination);
    let destination = destination.0.clone();
    let said = Arc::clone(said);
    move || {
        let mut ssh = Process::new("ssh");
        ssh.args(["-T", "-o", "BatchMode=yes"]).args(&sharing);
        ssh.args([&destination, "luneta", "bridge"]);
        // Where it can't be heard so, it says it on the daemon's stderr, as
        // its own runs.
        if let Ok(saying) = heard(&destination, &said) {
            ssh.stderr(saying);
        }
        ssh
    }
}

/// What the ssh runs to a machine said on their stderr.
#[derive(Default)]
struct Said {
    /// The lines of the last one heard to its end.
    lines: HashSet<String>,
    /// Hangs up once the last one to run has been heard to its end.
    last: Option<mpsc::Receiver<()>>,
}

/// Where an ssh to `destination` says what it says on its stderr: a pipe
/// that a thread reads until it ends, and says in the log, each line once,
/// and none that the one before it `said`. An ssh to a machine that stays
/// unreached runs every `AGAIN_MAX`, and says the same each time.
fn heard(destination: &str, said: &Arc<Mutex<Said>>) -> io::Result<PipeWriter> {
    let (heard, saying) = io::pipe()?;
    let (ends, ended) = mpsc::channel::<()>();
    let before = locked(said).last.replace(ended);
    let (destination, said) = (destination.to_owned(), Arc::clone(said));
    thread::Builder::new().name(format!("ssh {destination}")).spawn(move || {
        // What the one before said is known once it has been heard to its
        // end, as it has within moments of the next one's start, or not for
        // as long as what it left running holds its stderr.
        if let Some(before) = before {
            let _ = before.recv_timeout(HEARD_BEFORE);
        }
        let mut heard = BufReader::new(heard);
        let mut line = Vec::new();
        let mut lines = HashSet::new();
        // Until whatever holds its other end has ended: ssh, and anything it
        // left running.
        loop {
            line.clear();
            match (&mut heard).take(SAID_MAX).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {},
            }
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            if line.is_empty() || !lines.insert(line.clone()) {
                continue;
            }
            if !locked(&said).lines.contains(&line) {
                log::say(&format!("{destination}: {line}"));
            }
        }
        locked(&said).lines = lines;
        drop(ends);
    })?;
    Ok(saying)
}

/// What `mutex` holds, though a thread panicked as it held it: what is said
/// of an ssh is no more wrong for it.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// The machines `text` names, as the file, and how much of it does not
    /// read.
    fn named_in(text: &str) -> (Vec<String>, u32) {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, text).unwrap();
        let named = named(&file, &mut Troubles::default()).unwrap();
        (named.machines.iter().map(ToString::to_string).collect(), named.unread)
    }

    /// A line is a machine, without the white space around it, in the file's
    /// order and once: a blank line is none, and one ssh would take for an
    /// option, or for more than one word, is none and does not read.
    #[test]
    fn a_line_names_a_machine_once() {
        let text = "nixos\n\n  me@vps \t\nnixos\n-oProxyCommand=sh\nmy vps\nssh://me@vps:22\n";
        let (named, unread) = named_in(text);
        assert_eq!(named, ["nixos", "me@vps", "ssh://me@vps:22"]);
        assert_eq!(unread, 2);
    }

    /// No file names no machine, and a file names `HOSTS_MAX` at the most:
    /// the lines past those do not read.
    #[test]
    fn no_file_names_none_and_a_file_names_a_few_at_the_most() {
        let scratch = tempfile::tempdir().unwrap();
        let none = named(&scratch.path().join(FILE), &mut Troubles::default());
        assert_eq!(none, Some(Named::default()));
        let many: Vec<String> = (0..HOSTS_MAX + 3).map(|n| format!("host-{n}")).collect();
        let (named, unread) = named_in(&many.join("\n"));
        assert_eq!(named.len(), HOSTS_MAX);
        assert_eq!(named.last().unwrap(), &format!("host-{}", HOSTS_MAX - 1));
        assert_eq!(unread, 3);
    }

    /// A machine forgotten and added again has a watch that ends as the
    /// next begins: the daemon the later one reached is the one questions
    /// go to, whichever of the two heard last, and ends with it.
    #[test]
    fn a_watch_that_ends_leaves_a_later_one_s_bridge() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // A channel is made within a runtime, which it needs.
        let _entered = runtime.enter();
        let daemon = DaemonClient::new(Channel::from_static("http://[::1]:1").connect_lazy());
        let bridges = Arc::new(Bridges::default());
        let destination: Destination = "nixos".parse().unwrap();
        let of_watch = |watch| {
            let (bridges, destination) = (Arc::clone(&bridges), destination.clone());
            Bridged { bridges, destination, watch }
        };
        let (ending, later) = (of_watch(1), of_watch(2));
        later.reached(&daemon);
        ending.reached(&daemon);
        drop(ending);
        let watch = locked(&bridges.0).get(&destination).map(|(watch, _)| *watch);
        assert_eq!(watch, Some(2));
        drop(later);
        assert!(bridges.to(&destination).is_none());
    }

    /// Each ssh to a machine shares one socket, another machine's another,
    /// in luneta's runtime folder, with a `%` there spelled as ssh reads it
    /// back. A folder as long as macOS's `TMPDIR` has room for it; one that
    /// leaves ssh none to make it has none, and each ssh runs alone.
    #[test]
    fn each_ssh_to_a_machine_shares_a_socket_where_there_is_room_for_one() {
        let path = |folder: &str, destination: &str| -> Option<String> {
            let options = sharing(Path::new(folder), &destination.parse().unwrap());
            let options: Vec<String> =
                options.into_iter().map(|it| it.into_string().unwrap()).collect();
            options.iter().find_map(|it| it.strip_prefix("ControlPath=").map(str::to_owned))
        };
        let tmpdir = "/var/folders/7k/h0b8qt5x1y9b0b1k1q3j2n5h0000gn/T/luneta";
        let nixos = path(tmpdir, "nixos").unwrap();
        assert!(nixos.starts_with(&format!("{tmpdir}/ssh-")), "{nixos}");
        assert_eq!(path(tmpdir, "nixos"), Some(nixos.clone()));
        assert_ne!(path(tmpdir, "me@vps"), Some(nixos));
        let odd = path("/run/user/1000/50%/luneta", "nixos").unwrap();
        assert!(odd.starts_with("/run/user/1000/50%%/luneta/ssh-"), "{odd}");
        assert_eq!(path(&format!("/{}", "x".repeat(70)), "nixos"), None);
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
