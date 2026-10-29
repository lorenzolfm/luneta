//! lunetad as luneta reaches it: on its socket in the folder, started where
//! none answers, and started afresh in the place of an older one.
//!
//! luneta starts the lunetad beside it, which the same build made, or else
//! the first on the `PATH`. It starts it detached: in a session of its own,
//! in `/`, with nothing on its stdin and stdout, and its stderr in a log in
//! the folder. So it holds nothing of luneta's: a program reads what
//! `luneta watch` says to its end, and a daemon that held luneta's stdout
//! would keep it from ever answering.
//!
//! Two lunetas that find none in the same moment start one each, and the
//! folder's lock lets one through. The other ends at once, and both lunetas
//! reach the one that runs.
//!
//! A daemon makes way only for a newer one. Were it to make way for one of
//! any other version, two lunetas of two versions, each with its lunetad
//! beside it, would take turns at it for as long as both ask: a program
//! that watches runs a `luneta watch` for each answer, which knows nothing of
//! the one before it.
//!
//! A daemon that reads zellij's sessions in another folder than luneta's
//! zellij keeps them in, as one started with another `ZELLIJ_SOCKET_DIR`
//! does, luneta neither asks nor stops, whatever its version: its sessions
//! are not luneta's, and one started in its place would leave out those of
//! every luneta that reaches it, which would take what they kept of those
//! sessions for gone.

use std::cell::OnceCell;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::InfoRequest;
use lunetad::folder::{self, Folder, ANOTHER_RUNS, LOG};
use lunetad::program;
use rustix::process::{Pid, Signal};
use tonic::transport::Channel;

use crate::commands::causes;

/// The daemon's program, by the name the build gives it.
const LUNETAD: &str = "lunetad";

/// The version of luneta this is, which the daemons it starts are of too.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long luneta waits for a daemon to answer that it started, or that
/// another luneta is starting, and for one to end that it stopped. One that
/// starts reads what runs before it answers, which takes it a moment.
const START: Duration = Duration::from_secs(5);

/// How often luneta looks again while it waits.
const LOOK: Duration = Duration::from_millis(20);

/// How large the log grows before luneta empties it: each daemon it starts
/// writes on at its end.
const LOG_MAX: u64 = 1024 * 1024;

/// The most of what a daemon that did not start wrote that luneta reads.
const SAID_MAX: u64 = 4096;

/// How long `lunetad --version` has to answer.
const VERSION_LIMIT: Duration = Duration::from_secs(2);

/// Which daemon luneta wants.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The one that runs, if one does, whatever it is: to say what runs.
    Running,
    /// The one that runs, or one started where none does, or in the place of
    /// an older one, as an upgrade leaves running.
    Current,
}

/// A client of the daemon in the folder at `path`, as `wanted`, or why there
/// is none.
pub async fn reach(path: &Path, wanted: Reach) -> Result<DaemonClient<Channel>, String> {
    let folder = match folder::open(path).map_err(|refusal| refusal.to_string())? {
        Some(folder) => folder,
        None if wanted == Reach::Running => return Err(none_runs(path)),
        // No daemon has run yet, as one makes the folder as it starts. luneta
        // makes it first, as it reaches into no folder that it has not
        // checked.
        None => folder::make(path).map_err(|refusal| refusal.to_string())?,
    };
    let socket = folder.socket();
    let deadline = Instant::now() + START;
    let mut started: Option<Started> = None;
    // The daemon luneta has stopped, by its pid, and waits to end.
    let mut stopped: Option<u32> = None;
    // The daemon, by the pid it said, that the kernel said another listens in
    // the place of: one in another namespace of pids, which says the same
    // pid again, or one that has ended since it answered, and another
    // listens in its place, which answers next.
    let mut doubted: Option<u32> = None;
    // The release of the lunetad luneta starts, once asked.
    let starting = OnceCell::new();
    // Where luneta's zellij keeps its sessions.
    let own = lunetad::zellij_folder();
    loop {
        let why = match lunetad::connect(&folder).await {
            // `Running` starts none, so there is none to reap.
            Ok(daemon) if wanted == Reach::Running => return Ok(daemon),
            Ok(mut daemon) => match daemon.info(InfoRequest {}).await {
                Ok(info) => {
                    let info = info.into_inner();
                    let (version, pid) = (info.version, info.pid);
                    // A daemon older than the field says no folder, and is
                    // taken to read luneta's, as before.
                    let reads = info.zellij_folder.map(OsString::from_vec).map(PathBuf::from);
                    if stopped == Some(pid) {
                        format!("lunetad {version} (pid {pid}) was stopped, and has not ended")
                    } else if let Some(reads) = reads.filter(|reads| !same_folder(reads, &own)) {
                        reap(started);
                        return Err(format!(
                            "lunetad {version} (pid {pid}) reads zellij's sessions in {}, not in \
                             {}, where luneta's zellij keeps them",
                            reads.display(),
                            own.display()
                        ));
                    } else if !makes_way(&version, || *starting.get_or_init(release)) {
                        reap(started);
                        return Ok(daemon);
                    } else {
                        match listener(&folder, pid).await {
                            Listener::Other if doubted == Some(pid) => {
                                reap(started);
                                return Ok(daemon);
                            },
                            Listener::Other => {
                                doubted = Some(pid);
                                format!("lunetad {version} (pid {pid}) is not the one that listens")
                            },
                            // Another luneta stopped it, as the sidebars of an
                            // upgrade do at once.
                            Listener::Gone => {
                                format!("lunetad {version} (pid {pid}) has ended since it answered")
                            },
                            Listener::It => {
                                stop(pid)?;
                                stopped = Some(pid);
                                let older = format!("lunetad {version} (pid {pid})");
                                format!("{older} is older than luneta, and ends")
                            },
                        }
                    }
                },
                Err(status) => format!("{}: {}", socket.display(), causes(&status)),
            },
            Err(error) => format!("{}: {}", socket.display(), causes(&error)),
        };
        let held = || folder::held(&folder).map_err(|refusal| refusal.to_string());
        if wanted == Reach::Running {
            return Err(if held()? { why } else { none_runs(path) });
        }
        // The folder's lock is not looked at while a daemon luneta started is
        // starting: it is taken to look, and that daemon could find it taken.
        match &mut started {
            // One that holds the folder is starting, or ending: luneta waits
            // for it to answer, or to let the folder go.
            None => {
                if !held()? {
                    started = Some(start(&folder)?);
                }
            },
            Some(daemon) => match daemon.child.try_wait() {
                Ok(None) => {},
                // Another holds the folder, and luneta waits on it as above.
                Ok(Some(ended)) if ended.code() == Some(ANOTHER_RUNS.into()) => started = None,
                Ok(Some(ended)) => return Err(daemon.failed(ended)),
                Err(error) => return Err(format!("{LUNETAD}: {error}")),
            },
        }
        if Instant::now() >= deadline {
            let why = match &started {
                Some(daemon) => format!(
                    "the lunetad luneta started did not answer in {}s, and {} may say why: {why}",
                    START.as_secs(),
                    daemon.log.display()
                ),
                None => format!("no lunetad answered in {}s: {why}", START.as_secs()),
            };
            reap(started);
            return Err(why);
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// What luneta says where no daemon runs in the folder at `path`.
fn none_runs(path: &Path) -> String {
    format!("no lunetad runs in {}", path.display())
}

/// Whether `a` and `b` are one folder: by the same path, or by two that lead
/// to it, as a link and the folder it leads to do.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).is_ok_and(|a| fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// Waits for whatever luneta `started` as it ends, so that one that ends
/// while luneta runs on is not left a zombie. A thread that does not start
/// leaves it one.
fn reap(started: Option<Started>) {
    if let Some(Started { mut child, .. }) = started {
        let _ = thread::Builder::new().name(LUNETAD.to_owned()).spawn(move || child.wait());
    }
}

/// A lunetad that luneta started, and how long its log was as it did.
struct Started {
    child: Child,
    log: PathBuf,
    from: u64,
}

impl Started {
    /// Why it did not start, as it `ended`: the first line it wrote in its
    /// log, or else how it ended.
    fn failed(&self, ended: ExitStatus) -> String {
        let said = self.said().unwrap_or_default();
        match said.lines().map(str::trim).find(|line| !line.is_empty()) {
            Some(line) => format!("{LUNETAD} did not start: {line}"),
            None => format!(
                "{LUNETAD} did not start ({ended}), and wrote nothing in {}",
                self.log.display()
            ),
        }
    }

    /// What it wrote in its log, or the most of it that luneta reads.
    fn said(&self) -> io::Result<String> {
        let mut log = File::open(&self.log)?;
        log.seek(SeekFrom::Start(self.from))?;
        let mut said = Vec::new();
        log.take(SAID_MAX).read_to_end(&mut said)?;
        Ok(String::from_utf8_lossy(&said).into_owned())
    }
}

/// Starts lunetad, detached, to hold `folder`.
fn start(folder: &Folder) -> Result<Started, String> {
    let path = folder.path().join(LOG);
    let opened = || {
        let log = OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
        let mut from = log.metadata()?.len();
        if from > LOG_MAX {
            log.set_len(0)?;
            from = 0;
        }
        io::Result::Ok((log, from))
    };
    let (log, from) = opened().map_err(|error| format!("{}: {error}", path.display()))?;
    let program = lunetad();
    let child = Command::new(&program)
        .arg("--detached")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|error| format!("{} could not be started: {error}", program.display()))?;
    Ok(Started { child, log: path, from })
}

/// The lunetad that luneta starts: the one beside the file luneta is, which
/// the same build made, or else the first on the `PATH`.
fn lunetad() -> PathBuf {
    let beside = env::current_exe()
        .and_then(fs::canonicalize)
        .ok()
        .and_then(|luneta| Some(luneta.parent()?.join(LUNETAD)))
        .filter(|lunetad| lunetad.is_file());
    beside.unwrap_or_else(|| PathBuf::from(LUNETAD))
}

/// A release of luneta and lunetad, by its version, as `0.2.2`. Releases are
/// ordered, and a daemon makes way for a newer one alone.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Release(u64, u64, u64);

impl Release {
    /// The release `version` names, or none: a version of luneta's is three
    /// numbers and no more.
    fn parse(version: &str) -> Option<Release> {
        let mut numbers = version.split('.').map(|number| number.parse().ok());
        match (numbers.next(), numbers.next(), numbers.next(), numbers.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => {
                Some(Release(major, minor, patch))
            },
            _ => None,
        }
    }
}

/// Whether the daemon of the version `running` makes way for the lunetad
/// that luneta starts, of the release `starting` says: only where that one
/// is newer than it, and luneta too. A daemon as new as luneta makes way for
/// none, and so luneta asks that lunetad nothing while it is; and a lunetad
/// that is no newer than the daemon would be stopped in its turn by the next
/// luneta of the daemon's version. A daemon whose version luneta cannot read
/// is older than any.
fn makes_way(running: &str, starting: impl FnOnce() -> Option<Release>) -> bool {
    let running = Release::parse(running);
    let older = |than: Release| running.is_none_or(|running| running < than);
    Release::parse(VERSION).is_some_and(older) && starting().is_some_and(older)
}

/// The release of the lunetad that luneta starts, as it says to `--version`.
fn release() -> Option<Release> {
    let mut lunetad = Command::new(lunetad());
    lunetad.arg("--version");
    let said = program::run(&mut lunetad, VERSION_LIMIT).ok()?.answer().ok()?;
    let said = String::from_utf8_lossy(&said);
    Release::parse(said.trim().strip_prefix(LUNETAD)?.strip_prefix(' ')?)
}

/// Who listens in the folder, against the pid a daemon said is its own.
enum Listener {
    /// The daemon that said it.
    It,
    /// A process the kernel names otherwise, or not at all, as it does one in
    /// another namespace of pids.
    Other,
    /// None that luneta reaches, as the daemon has ended since it answered.
    Gone,
}

/// Who listens in `folder`, as the kernel says, against `pid`. luneta stops
/// no process on a daemon's word alone: one in another namespace of pids says
/// a pid that is another process's here. A daemon it cannot stop, it asks as
/// it is, but not one that has ended, which would fail it: the kernel names
/// none, or the one that listens in its place.
async fn listener(folder: &Folder, pid: u32) -> Listener {
    match lunetad::listening(folder).await {
        Ok(listening) if listening == Some(pid) => Listener::It,
        Ok(_) => Listener::Other,
        Err(_) => Listener::Gone,
    }
}

/// Has the daemon `pid` end, which the kernel says listens in the folder.
/// SIGTERM ends a daemon of any version, where an older one knows nothing of
/// how a newer luneta would ask it to.
fn stop(pid: u32) -> Result<(), String> {
    let id = i32::try_from(pid).ok().and_then(Pid::from_raw);
    let id = id.ok_or_else(|| format!("{LUNETAD} says its pid is {pid}, which none is"))?;
    match rustix::process::kill_process(id, Signal::TERM) {
        // One that has ended since it answered has nothing to stop.
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(format!("{LUNETAD} (pid {pid}) could not be stopped: {error}")),
    }
}
