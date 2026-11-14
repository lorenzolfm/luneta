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
use tokio::sync::mpsc as wakes;
use tokio::sync::oneshot;
use tokio::sync::watch::Receiver;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::api::daemon_client::DaemonClient;
use crate::api::remote::{self, unreached};
use crate::api;
use crate::client::{self, Unbridged};
use crate::core::Command;
use crate::log::{self, causes};
use crate::state;
use crate::watch::{self, Troubles};

pub const FILE: &str = "hosts";

pub const HOSTS_MAX: usize = 16;

const AGAIN_MAX: Duration = Duration::from_secs(30);

const SAID_MAX: u64 = 4096;

const HEARD_BEFORE: Duration = Duration::from_secs(1);

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

pub enum Reached {
    Reaching,
    Host(remote::Host),
    Unreached { since: SystemTime, why: String, stopped: Stopped },
}

impl Reached {
    pub fn to_api(&self, destination: &Destination) -> api::Remote {
        let reached = match self {
            Reached::Reaching => remote::Reached::Reaching(()),
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

#[derive(Default)]
pub struct Bridges(Mutex<HashMap<Destination, Bridge>>);

struct Bridge {
    watch: u64,
    daemon: Option<DaemonClient<Channel>>,
    wake: wakes::Sender<()>,
}

impl Bridges {
    pub fn to(&self, destination: &Destination) -> Option<DaemonClient<Channel>> {
        let bridges = locked(&self.0);
        let bridge = bridges.get(destination)?;
        if bridge.daemon.is_none() {
            let _ = bridge.wake.try_send(());
        }
        bridge.daemon.clone()
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
        let bridge = Bridge { watch, daemon: None, wake };
        locked(&bridges.0).insert(destination.clone(), bridge);
        Bridged { bridges, destination, watch, woken }
    }

    fn own<'a>(&self, bridges: &'a mut HashMap<Destination, Bridge>) -> Option<&'a mut Bridge> {
        bridges.get_mut(&self.destination).filter(|bridge| bridge.watch == self.watch)
    }

    fn agree(&self, seen: &Seen, daemon: Option<&DaemonClient<Channel>>) {
        if let Some(bridge) = self.own(&mut locked(&self.bridges.0)) {
            bridge.daemon = match seen {
                Seen::Answered { .. } | Seen::Broke { .. } => daemon.cloned(),
                Seen::Reaching { .. } | Seen::Unreached { .. } => None,
            };
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
    pub machines: Vec<Destination>,
    pub unread: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    Unstarted,
    Ended(Option<i32>),
    Said,
    Silent,
    Lost,
    Unwatched,
}

impl Stopped {
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
            Stopped::Unstarted => unreached::Stopped::Unstarted(()),
            Stopped::Ended(code) => unreached::Stopped::Ended(unreached::Ended { code }),
            Stopped::Said => unreached::Stopped::Said(()),
            Stopped::Silent => unreached::Stopped::Silent(()),
            Stopped::Lost => unreached::Stopped::Lost(()),
            Stopped::Unwatched => unreached::Stopped::Unwatched(()),
        }
    }
}

fn unreached(error: &(dyn Error + 'static)) -> (String, Stopped) {
    (causes(error), Stopped::of(error))
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
    let mut told = Named::default();
    let mut watched: Vec<(Destination, oneshot::Sender<()>)> = Vec::new();
    let mut unwatched: HashMap<Destination, SystemTime> = HashMap::new();
    let mut unwatchable = Troubles::default();
    let mut begun: u64 = 0;
    let tell = move |named: Option<Named>| {
        let named = match named {
            Some(named) => named,
            None => Named { machines: told.machines.clone(), unread: 1 },
        };
        if named != told {
            if core.send(Command::Hosts(named.clone())).is_err() {
                return false;
            }
            let machines = &named.machines;
            watched.retain(|(destination, _)| machines.contains(destination));
            unwatched.retain(|destination, _| machines.contains(destination));
            told = named;
        }
        for destination in &told.machines {
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
    let mut daemon = None;
    let mut seen = Seen::Reaching { since: SystemTime::now() };
    let said = Arc::default();
    loop {
        if standing.wait_for(|standing| *standing != Standing::Idle).await.is_err() {
            return;
        }
        bridged.awake();
        let question = standing.borrow().question(seen.after());
        let asked = asked(&destination, &mut daemon, question, &said, &folder).await;
        let (version, host) = match asked {
            Ok(answered) => answered,
            Err(why) => {
                let logged = matches!(&seen, Seen::Unreached { why: logged, .. } if *logged == why);
                seen = seen.failed(why);
                bridged.agree(&seen, daemon.as_ref());
                if let Seen::Unreached { since, why: (why, stopped), .. } = &seen {
                    if !logged {
                        log::say(&format!("{destination}: unreached: {why}"));
                    }
                    let (since, why, stopped) = (*since, why.clone(), *stopped);
                    if !told(&core, &destination, Reached::Unreached { since, why, stopped }) {
                        return;
                    }
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
        seen = Seen::Answered { after: version, at: SystemTime::now() };
        bridged.agree(&seen, daemon.as_ref());
        if told_already {
            continue;
        }
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
            sayings,
        } = host;
        let host = remote::Host {
            sessions,
            work_trees,
            agents,
            unread,
            pull_requests,
            unread_sockets,
            unread_files,
            sayings,
        };
        if !told(&core, &destination, Reached::Host(host)) {
            return;
        }
    }
}

enum Seen {
    Reaching { since: SystemTime },
    Answered { after: api::Version, at: SystemTime },
    Broke { after: api::Version, at: SystemTime },
    Unreached { since: SystemTime, failures: u32, why: (String, Stopped) },
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

    fn failed(self, why: (String, Stopped)) -> Seen {
        let failures = self.failures().saturating_add(1);
        match self {
            Seen::Answered { after, at } => Seen::Broke { after, at },
            Seen::Reaching { since }
            | Seen::Broke { at: since, .. }
            | Seen::Unreached { since, .. } => Seen::Unreached { since, failures, why },
        }
    }
}

fn told(core: &SyncSender<Command>, destination: &Destination, reached: Reached) -> bool {
    core.send(Command::Remote { destination: destination.clone(), reached }).is_ok()
}

async fn asked(
    destination: &Destination,
    daemon: &mut Option<DaemonClient<Channel>>,
    question: api::WatchRequest,
    said: &Arc<Mutex<Said>>,
    folder: &Path,
) -> Result<(api::Version, api::Host), (String, Stopped)> {
    let daemon = if let Some(daemon) = daemon {
        daemon
    } else {
        let reached = client::through(ssh(destination, said, folder)).await;
        daemon.insert(reached.map_err(|error| unreached(&error))?)
    };
    let watched = daemon.watch(question).await;
    let host = watched.map_err(|status| unreached(&status))?.into_inner();
    let unversioned = "an answer with no version, which this lunetad does not read";
    let version = host.version.ok_or_else(|| (unversioned.to_owned(), Stopped::Lost))?;
    Ok((version, host))
}

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
        if let Ok(saying) = heard(&destination, &said) {
            ssh.stderr(saying);
        }
        ssh
    }
}

#[derive(Default)]
struct Said {
    lines: HashSet<String>,
    last: Option<mpsc::Receiver<()>>,
}

fn heard(destination: &str, said: &Arc<Mutex<Said>>) -> io::Result<PipeWriter> {
    let (heard, saying) = io::pipe()?;
    let (ends, ended) = mpsc::channel::<()>();
    let before = locked(said).last.replace(ended);
    let (destination, said) = (destination.to_owned(), Arc::clone(said));
    thread::Builder::new().name(format!("ssh {destination}")).spawn(move || {
        if let Some(before) = before {
            let _ = before.recv_timeout(HEARD_BEFORE);
        }
        let mut heard = BufReader::new(heard);
        let mut line = Vec::new();
        let mut lines = HashSet::new();
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
    use std::time::Duration;

    use super::*;

    fn answered() -> Seen {
        Seen::Answered { after: api::Version { pid: 1, seq: 1 }, at: SystemTime::now() }
    }

    fn lost() -> Seen {
        let why = ("ssh: Connection refused".to_owned(), Stopped::Ended(Some(255)));
        Seen::Unreached { since: SystemTime::now(), failures: 2, why }
    }

    fn named_in(text: &str) -> (Vec<String>, u32) {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, text).unwrap();
        let named = named(&file, &mut Troubles::default()).unwrap();
        (named.machines.iter().map(ToString::to_string).collect(), named.unread)
    }

    #[test]
    fn a_line_names_a_machine_once() {
        let text = "nixos\n\n  me@vps \t\nnixos\n-oProxyCommand=sh\nmy vps\nssh://me@vps:22\n";
        let (named, unread) = named_in(text);
        assert_eq!(named, ["nixos", "me@vps", "ssh://me@vps:22"]);
        assert_eq!(unread, 2);
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
        assert!(bridges.to(&destination).is_some());
        drop(later);
        assert!(bridges.to(&destination).is_none());
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

        assert!(bridges.to(&destination).is_none());
        assert!(bridges.to(&destination).is_none());
        assert!(woke(&mut watch, Duration::from_secs(10)));
        assert!(!woke(&mut watch, Duration::from_millis(100)));

        assert!(bridges.to(&destination).is_none());
        watch.awake();
        assert!(!woke(&mut watch, Duration::from_millis(100)));

        let daemon = DaemonClient::new(Channel::from_static("http://[::1]:1").connect_lazy());
        watch.agree(&answered(), Some(&daemon));
        assert!(bridges.to(&destination).is_some());
        assert!(!woke(&mut watch, Duration::from_millis(100)));
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
