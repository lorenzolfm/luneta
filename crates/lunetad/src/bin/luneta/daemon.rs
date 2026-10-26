//! lunetad as luneta reaches it: on its socket in the folder, started where
//! none answers, and started afresh in the place of one of another version.
//!
//! luneta starts the lunetad beside it, which the same build made, or else
//! the first on the `PATH`. It starts it detached: in a session of its own,
//! in `/`, with nothing on its stdin and stdout, and its stderr in a log in
//! the folder. So it holds nothing of luneta's: zellij reads what a plugin's
//! command says to its end, and a daemon that held luneta's stdout would keep
//! `luneta watch` from ever answering.
//!
//! Two lunetas that find none in the same moment start one each, and the
//! folder's lock lets one through. The other ends at once, and both lunetas
//! reach the one that runs.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::InfoRequest;
use lunetad::folder::{self, Refusal, ANOTHER_RUNS, LOG, SOCKET};
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
    /// The one that runs, or one started where none does.
    Started,
    /// As `Started`, and one of this luneta's version in the place of one of
    /// another, as an upgrade leaves running.
    Current,
}

/// A client of the daemon in `folder`, as `wanted`, or why there is none.
pub async fn reach(folder: &Path, wanted: Reach) -> Result<DaemonClient<Channel>, String> {
    match folder::check(folder) {
        Ok(()) => {},
        // No daemon has run yet, as one makes the folder as it starts.
        Err(Refusal::Io(_, error)) if error.kind() == ErrorKind::NotFound => {},
        Err(refusal) => return Err(refusal.to_string()),
    }
    let socket = folder.join(SOCKET);
    let deadline = Instant::now() + START;
    let mut started: Option<Started> = None;
    // The daemon luneta has stopped, by its pid, and waits to end. It stops
    // one at the most: a luneta of the version it stopped would stop the next
    // in turn, and the two would take turns for good.
    let mut stopped: Option<u32> = None;
    // Whether the lunetad luneta starts is of its version, once asked.
    let mut current: Option<bool> = None;
    loop {
        let why = match lunetad::connect(&socket).await {
            Ok(daemon) if wanted != Reach::Current => {
                reap(started);
                return Ok(daemon);
            },
            Ok(mut daemon) => match daemon.info(InfoRequest {}).await {
                Ok(info) => {
                    let info = info.into_inner();
                    let (version, pid) = (info.version, info.pid);
                    if stopped == Some(pid) {
                        format!("lunetad {version} (pid {pid}) was stopped, and has not ended")
                    } else if version == VERSION
                        || stopped.is_some()
                        || !*current.get_or_insert_with(starts_current)
                    {
                        reap(started);
                        return Ok(daemon);
                    } else {
                        stop(pid)?;
                        stopped = Some(pid);
                        format!("lunetad {version} (pid {pid}) is of another version, and ends")
                    }
                },
                Err(status) => format!("{}: {}", socket.display(), causes(&status)),
            },
            Err(error) => format!("{}: {}", socket.display(), causes(&error)),
        };
        let held = || folder::held(folder).map_err(|refusal| refusal.to_string());
        if wanted == Reach::Running {
            let none = || format!("no lunetad runs in {}", folder.display());
            return Err(if held()? { why } else { none() });
        }
        // The folder's lock is not looked at while a daemon luneta started is
        // starting: it is taken to look, and that daemon could find it taken.
        match &mut started {
            // One that holds the folder is starting, or ending: luneta waits
            // for it to answer, or to let the folder go.
            None => {
                if !held()? {
                    started = Some(start(folder)?);
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
fn start(folder: &Path) -> Result<Started, String> {
    folder::make(folder).map_err(|refusal| refusal.to_string())?;
    let path = folder.join(LOG);
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

/// Whether the lunetad that luneta starts is of luneta's version. One of
/// another would be stopped in its turn by the next luneta to come.
fn starts_current() -> bool {
    let mut lunetad = Command::new(lunetad());
    lunetad.arg("--version");
    let said = program::run(&mut lunetad, VERSION_LIMIT).ok().and_then(|ran| ran.answer().ok());
    said.is_some_and(|said| String::from_utf8_lossy(&said).trim() == format!("{LUNETAD} {VERSION}"))
}

/// Has the daemon `pid` end. SIGTERM ends a daemon of any version, where an
/// older one knows nothing of how a newer luneta would ask it to.
fn stop(pid: u32) -> Result<(), String> {
    let id = i32::try_from(pid).ok().and_then(Pid::from_raw);
    let id = id.ok_or_else(|| format!("{LUNETAD} says its pid is {pid}, which none is"))?;
    match rustix::process::kill_process(id, Signal::TERM) {
        // One that has ended since it answered has nothing to stop.
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(format!("{LUNETAD} (pid {pid}) could not be stopped: {error}")),
    }
}
