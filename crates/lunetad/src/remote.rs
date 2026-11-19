use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, BufRead as _, BufReader, ErrorKind, PipeWriter, Read as _};
use std::iter;
use std::net::IpAddr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::str::FromStr;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use notify::RecursiveMode;
use tokio::sync::mpsc as wakes;
use tokio::sync::oneshot;
use tokio::sync::watch::{self as latest, Receiver};
use tokio::sync::Semaphore;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::api::daemon_client::DaemonClient;
use crate::api::remote::{self, unreached};
use crate::api;
use crate::client::{self, Unbridged};
use crate::core::Command;
use crate::log::{self, causes};
use crate::{release, state};
use crate::watch::{self, Troubles};

pub const FILE: &str = "hosts";

pub const HOSTS_MAX: usize = 16;

pub const THIS_MACHINE: &str = "it is this machine, which luneta lists first";

pub const HELD_MAX: usize = 12;

const AGAIN_MAX: Duration = Duration::from_secs(30);

const SAID_MAX: u64 = 4096;

const HEARD_BEFORE: Duration = Duration::from_secs(1);

const HEARD_LAST: Duration = Duration::from_secs(2);

const _: () = assert!(HEARD_LAST.as_millis() > HEARD_BEFORE.as_millis());

const SSH_SAID_CHARS_MAX: usize = 240;

const SHARED_FOR: u32 = 60;

const ALIVE_EVERY: u32 = 5;
const ALIVE_MISSES: u32 = 2;

const SOCKET_PATH_MAX: usize = 103;

const SOCKET_MAKING: usize = 17;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Destination(String);

impl Destination {
    #[must_use]
    pub fn said(&self, status: &Status) -> String {
        match status.code() {
            Code::Unimplemented => {
                format!("{self}: its lunetad, or this one, is older than what luneta asked of it")
            },
            _ if status.message().is_empty() => format!("{self}: lunetad: {}", status.code()),
            _ => format!("{self}: {}", status.message()),
        }
    }

    #[must_use]
    pub fn is_here(&self) -> bool {
        self.is_on(&node())
    }

    fn is_on(&self, node: &str) -> bool {
        let host = self.host();
        let host = host.strip_suffix('.').unwrap_or(host);
        let short = node.split('.').next().unwrap_or_default();
        let named = |name: &str| !name.is_empty() && host.eq_ignore_ascii_case(name);
        let last = host.rsplit('.').next().unwrap_or_default();
        let loopback = host.parse::<IpAddr>().is_ok_and(|address| {
            let address = address.to_canonical();
            address.is_loopback() || address.is_unspecified()
        });
        named(node) || named(short) || last.eq_ignore_ascii_case("localhost") || loopback
    }

    fn host(&self) -> &str {
        let Some(uri) = self.0.strip_prefix("ssh://") else {
            let host = self.0.rsplit_once('@').map_or(self.0.as_str(), |(_, host)| host);
            return host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
        };
        let authority = uri.split('/').next().unwrap_or_default();
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        match host.strip_prefix('[') {
            Some(bracketed) => bracketed.split_once(']').map_or(bracketed, |(host, _)| host),
            None => host.split_once(':').map_or(host, |(host, _)| host),
        }
    }
}

impl FromStr for Destination {
    type Err = String;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Far(Destination),
    Here(Destination),
}

impl Line {
    fn of(destination: Destination, node: &str) -> Line {
        if destination.is_on(node) { Line::Here(destination) } else { Line::Far(destination) }
    }

    #[must_use]
    pub fn destination(&self) -> &Destination {
        match self {
            Line::Far(destination) | Line::Here(destination) => destination,
        }
    }
}

fn node() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

pub enum Reached {
    Reaching,
    Host(remote::Host),
    Unreached { since: SystemTime, failed: Failed, projects: Option<api::Projects> },
}

impl Reached {
    pub fn to_api(&self, destination: &Destination) -> api::Remote {
        let reached = match self {
            Reached::Reaching => remote::Reached::Reaching(()),
            Reached::Host(host) => remote::Reached::Host(host.clone()),
            Reached::Unreached { since, failed, projects } => {
                let (stopped, ssh_said) = failed.stopped.to_api();
                remote::Reached::Unreached(remote::Unreached {
                    since: Some((*since).into()),
                    why: failed.why.clone(),
                    stopped: Some(stopped),
                    projects: projects.clone(),
                    ssh_said,
                })
            },
        };
        api::Remote { destination: destination.0.clone(), reached: Some(reached) }
    }
}

#[derive(Default)]
pub struct Bridges(Mutex<HashMap<Destination, Bridge>>);

struct Bridge {
    watch: u64,
    daemon: latest::Sender<Kept>,
    wake: wakes::Sender<()>,
    held: Arc<Semaphore>,
}

enum Kept {
    None,
    Coming,
    Daemon(DaemonClient<Channel>),
}

pub struct Bridging {
    pub daemon: DaemonClient<Channel>,
    pub held: Arc<Semaphore>,
}

impl Bridges {
    pub async fn to(&self, destination: &Destination) -> Option<Bridging> {
        let (mut kept, held) = {
            let bridges = locked(&self.0);
            let bridge = bridges.get(destination)?;
            if matches!(*bridge.daemon.borrow(), Kept::None) {
                let _ = bridge.wake.try_send(());
            }
            (bridge.daemon.subscribe(), Arc::clone(&bridge.held))
        };
        let kept = kept.wait_for(|kept| !matches!(kept, Kept::Coming)).await.ok()?;
        match &*kept {
            Kept::Daemon(daemon) => Some(Bridging { daemon: daemon.clone(), held }),
            Kept::None | Kept::Coming => None,
        }
    }
}

struct Bridged {
    bridges: Arc<Bridges>,
    destination: Destination,
    watch: u64,
    woken: wakes::Receiver<()>,
}

impl Bridged {
    fn new(bridges: Arc<Bridges>, destination: Destination, watch: u64) -> Self {
        let (wake, woken) = wakes::channel(1);
        let (daemon, held) = (latest::Sender::new(Kept::None), Arc::new(Semaphore::new(HELD_MAX)));
        let bridge = Bridge { watch, daemon, wake, held };
        locked(&bridges.0).insert(destination.clone(), bridge);
        Bridged { bridges, destination, watch, woken }
    }

    fn own<'a>(&self, bridges: &'a mut HashMap<Destination, Bridge>) -> Option<&'a mut Bridge> {
        bridges.get_mut(&self.destination).filter(|bridge| bridge.watch == self.watch)
    }

    fn agree(&self, seen: &Seen, daemon: Option<&DaemonClient<Channel>>) {
        let kept = match (seen, daemon) {
            (Seen::Answered { .. } | Seen::Broke { .. }, Some(daemon)) => {
                Kept::Daemon(daemon.clone())
            },
            (Seen::Answered { .. } | Seen::Broke { .. }, None)
            | (Seen::Reaching { .. } | Seen::Unreached { .. }, _) => Kept::None,
        };
        self.keep(kept);
    }

    fn coming(&self, seen: &Seen) {
        if let Seen::Broke { .. } = seen {
            self.keep(Kept::Coming);
        }
    }

    fn keep(&self, kept: Kept) {
        if let Some(bridge) = self.own(&mut locked(&self.bridges.0)) {
            bridge.daemon.send_replace(kept);
        }
    }

    async fn waits(&mut self, wait: Duration) {
        let until = tokio::time::Instant::now() + wait;
        if let Ok(Some(())) = tokio::time::timeout_at(until, self.woken.recv()).await {
            return;
        }
        tokio::time::sleep_until(until).await;
    }

    fn awake(&mut self) {
        let _ = self.woken.try_recv();
    }
}

impl Drop for Bridged {
    fn drop(&mut self) {
        let mut bridges = locked(&self.bridges.0);
        if self.own(&mut bridges).is_some() {
            bridges.remove(&self.destination);
        }
    }
}

#[must_use]
pub fn sharing(folder: &Path, destination: &Destination) -> Vec<OsString> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in destination.0.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name = format!("/ssh-{hash:016x}");
    let folder = folder.as_os_str().as_bytes();
    let too_long = folder.len() + name.len() + SOCKET_MAKING > SOCKET_PATH_MAX;
    if too_long || folder.iter().copied().any(misread) {
        return Vec::new();
    }
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

fn misread(byte: u8) -> bool {
    byte == b' ' || byte.is_ascii_control() || matches!(byte, b'\'' | b'"' | b'\\')
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    pub lines: Vec<Line>,
    pub unread: u32,
}

impl Named {
    fn far(&self) -> impl Iterator<Item = &Destination> {
        self.lines.iter().filter_map(|line| match line {
            Line::Far(destination) => Some(destination),
            Line::Here(_) => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    why: String,
    stopped: Stopped,
}

impl Failed {
    fn of(why: String, stopped: Stopped) -> Self {
        Failed { why, stopped }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stopped {
    Unbridged(Ssh, Option<SshSaid>),
    Lost,
    Unwatched,
    Unmatched,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ssh {
    Unstarted,
    Ended(Option<i32>),
    Said,
    Silent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SshSaid(String);

impl Stopped {
    fn of(error: &(dyn Error + 'static)) -> Stopped {
        let mut sources = iter::successors(Some(error), |&error| error.source());
        let ssh = match sources.find_map(|error| error.downcast_ref::<Unbridged>()) {
            Some(Unbridged::Io(_)) => Ssh::Unstarted,
            Some(Unbridged::Ended(ended)) => Ssh::Ended(ended.code()),
            Some(Unbridged::Said(_)) => Ssh::Said,
            Some(Unbridged::Silent) => Ssh::Silent,
            None => return Stopped::Lost,
        };
        Stopped::Unbridged(ssh, None)
    }

    fn to_api(&self) -> (unreached::Stopped, Option<String>) {
        match self {
            Stopped::Unbridged(ssh, said) => {
                (ssh.to_api(), said.as_ref().map(|said| said.0.clone()))
            },
            Stopped::Lost => (unreached::Stopped::Lost(()), None),
            Stopped::Unwatched => (unreached::Stopped::Unwatched(()), None),
            Stopped::Unmatched => (unreached::Stopped::Unmatched(()), None),
        }
    }
}

impl Ssh {
    fn to_api(self) -> unreached::Stopped {
        match self {
            Ssh::Unstarted => unreached::Stopped::Unstarted(()),
            Ssh::Ended(code) => unreached::Stopped::Ended(unreached::Ended { code }),
            Ssh::Said => unreached::Stopped::Said(()),
            Ssh::Silent => unreached::Stopped::Silent(()),
        }
    }
}

impl SshSaid {
    fn of(line: &str) -> Option<SshSaid> {
        let mut kept = line.chars().filter(|char| !char.is_control());
        let mut said: String = kept.by_ref().take(SSH_SAID_CHARS_MAX).collect();
        if said.is_empty() {
            return None;
        }
        if kept.next().is_some() {
            said.pop();
            said.push('…');
        }
        assert!(said.chars().count() <= SSH_SAID_CHARS_MAX, "{said:?} is past the bound");
        Some(SshSaid(said))
    }
}

fn unreached(error: &(dyn Error + 'static)) -> Failed {
    Failed::of(causes(error), Stopped::of(error))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    #[default]
    Idle,
    Kept,
    Window,
}

impl Standing {
    #[must_use]
    pub fn of(kept: bool, window: bool) -> Self {
        match (kept, window) {
            (false, _) => Standing::Idle,
            (true, false) => Standing::Kept,
            (true, true) => Standing::Window,
        }
    }

    #[must_use]
    pub fn question(self, after: Option<api::Version>) -> api::WatchRequest {
        let shown = self == Standing::Window;
        api::WatchRequest { after, kept: Some(self != Standing::Idle), shown }
    }
}

pub fn follow(
    core: SyncSender<Command>,
    standing: Receiver<Standing>,
    sweep: Duration,
    folder: PathBuf,
    bridges: Arc<Bridges>,
) -> io::Result<()> {
    let file = match state::path(FILE) {
        Ok(file) => file,
        Err(unset) => {
            log::say(&format!("no other machine is watched: {unset}"));
            let _ = core.send(Command::Hosts(Named { lines: Vec::new(), unread: 1 }));
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
    let mut told = Named::default();
    let mut watched: Vec<(Destination, oneshot::Sender<()>)> = Vec::new();
    let mut unwatched: HashMap<Destination, SystemTime> = HashMap::new();
    let mut unwatchable = Troubles::default();
    let mut begun: u64 = 0;
    let tell = move |named: Option<Named>| {
        let named = match named {
            Some(named) => named,
            None => Named { lines: told.lines.clone(), unread: 1 },
        };
        if named != told {
            if core.send(Command::Hosts(named.clone())).is_err() {
                return false;
            }
            watched.retain(|(destination, _)| named.far().any(|far| far == destination));
            unwatched.retain(|destination, _| named.far().any(|far| far == destination));
            told = named;
        }
        for destination in told.far() {
            if watched.iter().any(|(watched, _)| watched == destination) {
                continue;
            }
            let bridges = Arc::clone(&bridges);
            begun += 1;
            let daemon = Bridged::new(bridges, destination.clone(), begun);
            match watch(daemon, core.clone(), standing.clone(), folder.clone()) {
                Ok(ends) => {
                    unwatched.remove(destination);
                    watched.push((destination.clone(), ends));
                },
                Err(error) => {
                    unwatchable.say(format!("{destination} is not watched: {error}"));
                    let since = unwatched.entry(destination.clone());
                    let since = *since.or_insert_with(SystemTime::now);
                    let why = format!("it is not watched: {error}");
                    let failed = Failed::of(why, Stopped::Unwatched);
                    let reached = Reached::Unreached { since, failed, projects: None };
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

fn named(path: &Path, troubles: &mut Troubles) -> Option<Named> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Some(Named::default()),
        Err(error) => {
            troubles.say(format!("{}: {error}", path.display()));
            return None;
        },
    };
    Some(named_by(&text, |what| {
        troubles.say(format!("{}: {what}", path.display()));
    }))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub lines: Vec<Line>,
    pub unread: Vec<String>,
}

#[must_use]
pub fn parsed(text: &str) -> Parsed {
    parsed_on(text, &node())
}

fn parsed_on(text: &str, node: &str) -> Parsed {
    let mut seen = HashSet::new();
    let mut parsed = Parsed::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match line.parse::<Destination>() {
            Ok(destination) if seen.insert(destination.clone()) => {
                parsed.lines.push(Line::of(destination, node));
            },
            Ok(_) => {},
            Err(why) => parsed.unread.push(why),
        }
    }
    parsed
}

pub fn named_by(text: &str, mut trouble: impl FnMut(String)) -> Named {
    let Parsed { lines, unread } = parsed(text);
    let mut named = Named { lines, unread: 0 };
    for why in unread {
        trouble(format!("{why}, and is left out"));
        named.unread = named.unread.saturating_add(1);
    }
    if named.lines.len() > HOSTS_MAX {
        let past = named.lines.drain(HOSTS_MAX..).map(|line| line.destination().to_string());
        let past: Vec<String> = past.collect();
        let many = u32::try_from(past.len()).unwrap_or(u32::MAX);
        named.unread = named.unread.saturating_add(many);
        let past = past.join(", ");
        trouble(format!("{past} are left out, as the daemon watches {HOSTS_MAX} at the most"));
    }
    for line in &named.lines {
        if let Line::Here(destination) = line {
            trouble(format!("{destination}: {THIS_MACHINE}, so no ssh runs to it"));
        }
    }
    named
}

fn watch(
    bridged: Bridged,
    core: SyncSender<Command>,
    standing: Receiver<Standing>,
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
        runtime.block_on(async move {
            tokio::spawn(watched(bridged, core, standing, folder));
            let _ = ending.await;
        });
    })?;
    let ended = |_| io::Error::other("its thread ended before it had a runtime");
    building.recv().map_err(ended)??;
    Ok(ends)
}

async fn watched(
    mut bridged: Bridged,
    core: SyncSender<Command>,
    mut standing: Receiver<Standing>,
    folder: PathBuf,
) {
    let destination = bridged.destination.clone();
    let mut connected = None;
    let mut seen = Seen::Reaching { since: SystemTime::now() };
    let said = Arc::default();
    loop {
        if standing.wait_for(|standing| *standing != Standing::Idle).await.is_err() {
            return;
        }
        bridged.awake();
        let question = standing.borrow().question(seen.after());
        let asked = asked(&bridged, &seen, &mut connected, question, &said, &folder).await;
        let (version, host) = match asked {
            Ok(answered) => answered,
            Err(failed) => {
                let logged = match &seen {
                    Seen::Unreached { failed: logged, .. } => logged.why == failed.why,
                    Seen::Reaching { .. } | Seen::Answered { .. } | Seen::Broke { .. } => false,
                };
                seen = seen.failed(failed);
                bridged.agree(&seen, connected.as_ref());
                if let Seen::Unreached { failed, .. } = &seen
                    && !logged
                {
                    log::say(&format!("{destination}: unreached: {}", failed.why));
                }
                if let Some(reached) = seen.unreached()
                    && !told(&core, &destination, reached)
                {
                    return;
                }
                bridged.waits(again(seen.failures())).await;
                continue;
            },
        };
        if let Seen::Unreached { since, .. } = &seen {
            let unreached_for = since.elapsed().unwrap_or_default().as_secs();
            log::say(&format!("{destination}: reached, after {unreached_for}s unreached"));
        }
        let told_already = seen.after() == Some(version);
        let projects = host.projects.clone();
        seen = Seen::Answered { after: version, at: SystemTime::now(), projects };
        bridged.agree(&seen, connected.as_ref());
        if told_already {
            continue;
        }
        let api::Host {
            version: _,
            git_statuses,
            agents,
            unread,
            pull_requests,
            unread_files,
            remotes: _,
            unread_hosts: _,
            sayings,
            projects,
        } = host;
        let host = remote::Host {
            git_statuses,
            agents,
            unread,
            pull_requests,
            unread_files,
            sayings,
            projects,
        };
        if !told(&core, &destination, Reached::Host(host)) {
            return;
        }
    }
}

enum Seen {
    Reaching { since: SystemTime },
    Answered { after: api::Version, at: SystemTime, projects: Option<api::Projects> },
    Broke { after: api::Version, at: SystemTime, projects: Option<api::Projects> },
    Unreached { since: SystemTime, failures: u32, failed: Failed, projects: Option<api::Projects> },
}

impl Seen {
    fn after(&self) -> Option<api::Version> {
        match self {
            Seen::Answered { after, .. } | Seen::Broke { after, .. } => Some(*after),
            Seen::Reaching { .. } | Seen::Unreached { .. } => None,
        }
    }

    fn failures(&self) -> u32 {
        match self {
            Seen::Reaching { .. } | Seen::Answered { .. } => 0,
            Seen::Broke { .. } => 1,
            Seen::Unreached { failures, .. } => *failures,
        }
    }

    fn failed(self, failed: Failed) -> Seen {
        let failures = self.failures().saturating_add(1);
        match self {
            Seen::Answered { after, at, projects } => Seen::Broke { after, at, projects },
            Seen::Reaching { since } => Seen::Unreached { since, failures, failed, projects: None },
            Seen::Broke { at: since, projects, .. } | Seen::Unreached { since, projects, .. } => {
                Seen::Unreached { since, failures, failed, projects }
            },
        }
    }

    fn unreached(&self) -> Option<Reached> {
        let Seen::Unreached { since, failed, projects, .. } = self else {
            return None;
        };
        let (since, failed, projects) = (*since, failed.clone(), projects.clone());
        Some(Reached::Unreached { since, failed, projects })
    }
}

fn told(core: &SyncSender<Command>, destination: &Destination, reached: Reached) -> bool {
    core.send(Command::Remote { destination: destination.clone(), reached }).is_ok()
}

async fn asked(
    bridged: &Bridged,
    seen: &Seen,
    connected: &mut Option<DaemonClient<Channel>>,
    question: api::WatchRequest,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> Result<(api::Version, api::Host), Failed> {
    let daemon = if let Some(daemon) = connected {
        daemon
    } else {
        bridged.coming(seen);
        let daemon = connected.insert(matched(&bridged.destination, said, folder).await?);
        bridged.agree(seen, Some(&*daemon));
        daemon
    };
    let watched = daemon.watch(question).await;
    let answered = watched.map_err(|status| unreached(&status)).and_then(|host| {
        let host = host.into_inner();
        let unversioned = "an answer with no version, which this lunetad does not read";
        let version =
            host.version.ok_or_else(|| Failed::of(unversioned.to_owned(), Stopped::Lost))?;
        Ok((version, host))
    });
    if answered.is_err() {
        *connected = None;
    }
    answered
}

async fn matched(
    destination: &Destination,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> Result<DaemonClient<Channel>, Failed> {
    let (ssh, last) = ssh(destination, said, folder);
    let mut daemon = match client::through(ssh).await {
        Ok(daemon) => daemon,
        Err(error) => {
            let mut failed = unreached(&error);
            if let Stopped::Unbridged(_, ssh_said) = &mut failed.stopped
                && let Some(last) = last
            {
                match tokio::time::timeout(HEARD_LAST, last).await {
                    Ok(said) => *ssh_said = said.ok(),
                    Err(_) => log::say(&format!(
                        "{destination}: ssh's stderr was open {}s after it failed, so what it said \
                         last is not shown",
                        HEARD_LAST.as_secs()
                    )),
                }
            }
            return Err(failed);
        },
    };
    let info = daemon.info(()).await.map_err(|status| unreached(&status))?.into_inner();
    match release::of(&info.version, info.built) {
        release::Of::Own => Ok(daemon),
        release::Of::Other(other) => {
            Err(Failed::of(unmatched(&other, info.pid), Stopped::Unmatched))
        },
    }
}

fn unmatched(other: &release::Other, pid: u32) -> String {
    let own = release::SAID.as_str();
    format!("lunetad {other} (pid {pid}) is another build than this one, {own}")
}

fn ssh(
    destination: &Destination,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> (Process, Option<oneshot::Receiver<SshSaid>>) {
    let mut ssh = Process::new("ssh");
    ssh.args(["-T", "-o", "BatchMode=yes"]).args(sharing(folder, destination));
    ssh.args([&destination.0, "luneta", "bridge"]);
    let Ok((saying, last)) = heard(&destination.0, said) else {
        return (ssh, None);
    };
    ssh.stderr(saying);
    (ssh, Some(last))
}

#[derive(Default)]
struct Said {
    lines: HashSet<String>,
    last: Option<mpsc::Receiver<()>>,
}

fn heard(
    destination: &str,
    said: &Arc<Mutex<Said>>,
) -> io::Result<(PipeWriter, oneshot::Receiver<SshSaid>)> {
    let (heard, saying) = io::pipe()?;
    let (ends, ended) = mpsc::channel::<()>();
    let (says_last, last) = oneshot::channel();
    let before = locked(said).last.replace(ended);
    let (destination, said) = (destination.to_owned(), Arc::clone(said));
    thread::Builder::new().name(format!("ssh {destination}")).spawn(move || {
        if let Some(before) = before {
            let _ = before.recv_timeout(HEARD_BEFORE);
        }
        let mut heard = BufReader::new(heard);
        let mut line = Vec::new();
        let mut lines = HashSet::new();
        let mut last = None;
        loop {
            line.clear();
            match (&mut heard).take(SAID_MAX).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {},
            }
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            if line.is_empty() {
                continue;
            }
            if let Some(said) = SshSaid::of(&line) {
                last = Some(said);
            }
            if !lines.insert(line.clone()) {
                continue;
            }
            if !locked(&said).lines.contains(&line) {
                log::say(&format!("{destination}: {line}"));
            }
        }
        locked(&said).lines = lines;
        if let Some(last) = last {
            let _ = says_last.send(last);
        }
        drop(ends);
    })?;
    Ok((saying, last))
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn again(failures: u32) -> Duration {
    let Some(doubled) = failures.checked_sub(2) else {
        return Duration::ZERO;
    };
    let secs = 1_u64.checked_shl(doubled).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(AGAIN_MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::time::Duration;

    use super::*;

    fn answered() -> Seen {
        answered_with(None)
    }

    fn answered_with(projects: Option<api::Projects>) -> Seen {
        Seen::Answered { after: api::Version { pid: 1, seq: 1 }, at: SystemTime::now(), projects }
    }

    fn refused() -> Failed {
        let stopped = Stopped::Unbridged(Ssh::Ended(Some(255)), None);
        Failed::of("ssh: Connection refused".to_owned(), stopped)
    }

    fn lost() -> Seen {
        Seen::Unreached { since: SystemTime::now(), failures: 2, failed: refused(), projects: None }
    }

    fn named_in(text: &str) -> (Vec<String>, u32) {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, text).unwrap();
        let named = named(&file, &mut Troubles::default()).unwrap();
        (named.lines.iter().map(|line| line.destination().to_string()).collect(), named.unread)
    }

    #[test]
    fn a_line_names_a_machine_once() {
        let text = "nixos\n\n  me@vps \t\nnixos\n-oProxyCommand=sh\nmy vps\nssh://me@vps:22\n";
        let (named, unread) = named_in(text);
        assert_eq!(named, ["nixos", "me@vps", "ssh://me@vps:22"]);
        assert_eq!(unread, 2);
    }

    #[test]
    fn a_destination_names_this_machine_by_its_node_name_or_a_loopback_name() {
        let here = [
            "nixos",
            "NixOS",
            "lorenzo@nixos",
            "nixos.lan",
            "nixos.",
            "me@host@nixos",
            "ssh://lorenzo@nixos:2222",
            "ssh://nixos/",
            "localhost",
            "root@LOCALHOST",
            "box.localhost",
            "127.0.0.2",
            "lorenzo@[::1]",
            "ssh://[::ffff:127.0.0.1]:22",
            "0.0.0.0",
        ];
        for destination in here {
            let named: Destination = destination.parse().unwrap();
            assert!(named.is_on("nixos.lan"), "{destination} is this machine");
        }
        let elsewhere = [
            "nixos-2",
            "nixos.example.com",
            "me@vps",
            "ssh://nixos@vps:22",
            "localhost-box",
            "10.0.1.1",
            "[::2]",
            "lan",
        ];
        for destination in elsewhere {
            let named: Destination = destination.parse().unwrap();
            assert!(!named.is_on("nixos.lan"), "{destination} is another machine");
        }
        assert!(!"x".parse::<Destination>().unwrap().is_on(""));
        for destination in ["nixos", "me@nixos", "NIXOS."] {
            let named: Destination = destination.parse().unwrap();
            assert!(named.is_on("nixos"), "{destination} is this machine");
        }
        assert!(!"nixos.lan".parse::<Destination>().unwrap().is_on("nixos"));
    }

    #[test]
    fn a_line_naming_this_machine_is_parsed_apart_from_the_others() {
        let parsed = parsed_on("me@nixos\nvps\nlocalhost\nnixos\nNIXOS\ntwo words\n", "nixos.lan");
        let machine = |name: &str| name.parse::<Destination>().unwrap();
        let lines = [
            Line::Here(machine("me@nixos")),
            Line::Far(machine("vps")),
            Line::Here(machine("localhost")),
            Line::Here(machine("nixos")),
            Line::Here(machine("NIXOS")),
        ];
        assert_eq!(parsed.lines, lines);
        assert_eq!(parsed.unread, ["\"two words\" is more than one word"]);
    }

    #[test]
    fn a_line_naming_this_machine_is_said_and_is_no_line_left_out() {
        let mut said = Vec::new();
        let named = named_by("localhost\nvps\n", |what| said.push(what));
        assert_eq!(named.far().collect::<Vec<_>>(), [&"vps".parse::<Destination>().unwrap()]);
        assert_eq!(named.unread, 0);
        assert_eq!(said, [format!("localhost: {THIS_MACHINE}, so no ssh runs to it")]);
    }

    #[test]
    fn what_ssh_says_is_shown_without_its_controls_and_up_to_a_bound() {
        let shown = |line: &str| SshSaid::of(line).map(|said| said.0);
        let colored = shown("\u{1b}[31mPermission denied\u{7}");
        assert_eq!(colored.as_deref(), Some("[31mPermission denied"));
        assert_eq!(shown("\u{1b}\u{7}"), None);
        for long in [SSH_SAID_CHARS_MAX + 1, SSH_SAID_CHARS_MAX + 9] {
            let cut = shown(&"a".repeat(long));
            assert_eq!(cut, Some(format!("{}…", "a".repeat(SSH_SAID_CHARS_MAX - 1))));
        }
        let most = "é".repeat(SSH_SAID_CHARS_MAX);
        assert_eq!(shown(&most), Some(most));
    }

    #[test]
    fn the_line_ssh_said_last_is_the_last_with_something_shown_in_it() {
        let said = Arc::new(Mutex::new(Said::default()));
        let (mut saying, last) = heard("nixos", &said).unwrap();
        saying.write_all(b"Warning: added\r\nPermission denied (publickey).\r\n\x1b\n\n").unwrap();
        drop(saying);
        assert_eq!(last.blocking_recv().unwrap().0, "Permission denied (publickey).");

        let (mut saying, last) = heard("nixos", &said).unwrap();
        saying.write_all(b"\x07\n").unwrap();
        drop(saying);
        assert!(last.blocking_recv().is_err(), "a line of controls alone is no line");
    }

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

    #[test]
    fn a_watch_that_ends_leaves_a_later_one_s_bridge() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _entered = runtime.enter();
        let daemon = DaemonClient::new(Channel::from_static("http://[::1]:1").connect_lazy());
        let bridges = Arc::new(Bridges::default());
        let destination: Destination = "nixos".parse().unwrap();
        let of_watch = |watch| Bridged::new(Arc::clone(&bridges), destination.clone(), watch);
        let ending = of_watch(1);
        let later = of_watch(2);
        later.agree(&answered(), Some(&daemon));
        ending.agree(&answered(), Some(&daemon));
        ending.agree(&lost(), None);
        drop(ending);
        let watch = locked(&bridges.0).get(&destination).map(|bridge| bridge.watch);
        assert_eq!(watch, Some(2));
        assert!(runtime.block_on(bridges.to(&destination)).is_some());
        drop(later);
        assert!(runtime.block_on(bridges.to(&destination)).is_none());
    }

    #[test]
    fn a_question_for_a_machine_not_reached_wakes_its_watch_once() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _entered = runtime.enter();
        let bridges = Arc::new(Bridges::default());
        let destination: Destination = "nixos".parse().unwrap();
        let mut watch = Bridged::new(Arc::clone(&bridges), destination.clone(), 1);
        let woke = |watch: &mut Bridged, within: Duration| {
            let waits = tokio::time::timeout(within, watch.waits(Duration::from_secs(60)));
            runtime.block_on(waits).is_ok()
        };

        assert!(runtime.block_on(bridges.to(&destination)).is_none());
        assert!(runtime.block_on(bridges.to(&destination)).is_none());
        assert!(woke(&mut watch, Duration::from_secs(10)));
        assert!(!woke(&mut watch, Duration::from_millis(100)));

        assert!(runtime.block_on(bridges.to(&destination)).is_none());
        watch.awake();
        assert!(!woke(&mut watch, Duration::from_millis(100)));

        let daemon = DaemonClient::new(Channel::from_static("http://[::1]:1").connect_lazy());
        watch.agree(&answered(), Some(&daemon));
        assert!(runtime.block_on(bridges.to(&destination)).is_some());
        assert!(!woke(&mut watch, Duration::from_millis(100)));
    }

    #[test]
    fn a_machine_lost_keeps_the_projects_last_heard_and_one_never_reached_has_none() {
        let shown = |seen: &Seen| {
            let reached = seen.unreached()?.to_api(&"nixos".parse().unwrap()).reached;
            let Some(remote::Reached::Unreached(unreached)) = reached else {
                panic!("{reached:?} is not unreached");
            };
            Some(unreached.projects)
        };
        let project = api::Project { id: 7, name: "luneta".to_owned(), ..api::Project::default() };
        let kept = api::projects::Kept { projects: vec![project], version: None };
        let heard = Some(api::Projects { held: Some(api::projects::Held::Kept(kept)) });

        let broke = answered_with(heard.clone()).failed(refused());
        assert_eq!(shown(&broke), None);
        let unreached = broke.failed(refused());
        assert_eq!(shown(&unreached), Some(heard.clone()));
        assert_eq!(shown(&unreached.failed(refused())), Some(heard));

        let never = Seen::Reaching { since: SystemTime::now() }.failed(refused());
        assert_eq!(shown(&never), Some(None));
        assert_eq!(shown(&never.failed(refused())), Some(None));
    }

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
        assert_eq!(path(&format!("/{}", "x".repeat(70)), "nixos"), None);
    }

    #[test]
    fn ssh_takes_what_it_is_told_to_share_on() {
        let folders = [
            ("/run/user/1000/luneta", true),
            ("/run/user/1000/50%/luneta", true),
            ("/run/user/1000/two words/luneta", false),
            ("/run/user/1000/a\ttab/luneta", false),
            ("/run/user/1000/it's/luneta", false),
            ("/run/user/1000/\"quoted\"/luneta", false),
            (r"/run/user/1000/back\\slashes/luneta", false),
        ];
        let destination: Destination = "nixos".parse().unwrap();
        for (folder, shares) in folders {
            let mut ssh = Process::new("ssh");
            ssh.args(["-G", "-F", "/dev/null"]);
            ssh.args(sharing(Path::new(folder), &destination)).arg("nixos");
            let read = match ssh.output() {
                Ok(read) => read,
                Err(error) if error.kind() == ErrorKind::NotFound => return,
                Err(error) => panic!("ssh: {error}"),
            };
            let said = String::from_utf8_lossy(&read.stderr);
            assert!(read.status.success(), "{folder:?}: {said}");
            let options = String::from_utf8(read.stdout).unwrap();
            let socket = options.lines().find_map(|line| line.strip_prefix("controlpath "));
            let ours = format!("{folder}/ssh-");
            if shares {
                let in_folder = socket.is_some_and(|socket| socket.starts_with(&ours));
                assert!(in_folder, "{folder:?}: {socket:?}");
            } else {
                assert_eq!(socket, None, "{folder:?}");
            }
        }
    }

    #[test]
    fn ssh_runs_again_after_longer_each_time_it_fails() {
        let secs: Vec<u64> = (1..=9).map(|failures| again(failures).as_secs()).collect();
        assert_eq!(secs, [0, 1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(again(u32::MAX), Duration::from_secs(30));
    }
}
