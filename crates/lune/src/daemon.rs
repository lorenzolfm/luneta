use std::cell::OnceCell;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::pin::{Pin, pin};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use luned::api::InfoResponse;
use luned::folder::{self, ANOTHER_RUNS, Folder, LOCK, LOG, SOCKET};
use luned::release::{self, Build, OWN, Other};
use luned::terminate::LUNED;
use luned::{
    FolderMark, FolderNames, FolderWatch, LISTENING, PeerPid, ProcessExit, ReadyLine, Shared, log,
    program,
};
use rustix::process::{Pid, Signal};

use crate::commands::causes;

pub const START: Duration = Duration::from_secs(5);

const HOLD_FALLBACK: Duration = Duration::from_millis(100);

const BIND_RETRY: Duration = Duration::from_millis(5);

const BIND_RETRIES_MAX: u8 = 2;

const REACH_ATTEMPTS_MAX: usize = 512;

const START_SPAWNS_MAX: u8 = 32;

const FOLDER_NAMES: FolderNames =
    FolderNames::try_new(SOCKET, LOCK).expect("luned socket and lock names are valid");

const _: () = assert!(REACH_ATTEMPTS_MAX as u128 > START.as_millis() / HOLD_FALLBACK.as_millis());
const _: () = assert!(START_SPAWNS_MAX > 0);

const LOG_MAX: u64 = 1024 * 1024;

const SAID_MAX: u64 = 4096;

pub const VERSION_LIMIT: Duration = Duration::from_secs(2);

pub const NONE_RUNS: u8 = 5;

pub struct Current {
    pub folder: Folder,
    pub daemon: Shared,
}

pub enum Unreached {
    NoneRuns(PathBuf),
    Failed(String),
}

impl Unreached {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Unreached::NoneRuns(_) => ExitCode::from(NONE_RUNS),
            Unreached::Failed(_) => ExitCode::FAILURE,
        }
    }
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreached::NoneRuns(path) => write!(f, "no luned runs in {}", path.display()),
            Unreached::Failed(why) => f.write_str(why),
        }
    }
}

impl From<Unreached> for String {
    fn from(unreached: Unreached) -> String {
        unreached.to_string()
    }
}

pub async fn running(path: &Path) -> Result<Shared, Unreached> {
    let refused = |refusal: folder::Refusal| Unreached::Failed(refusal.to_string());
    let folder = folder::open(path).map_err(refused)?;
    let folder = folder.ok_or_else(|| Unreached::NoneRuns(path.to_owned()))?;
    match luned::reach(&folder).await {
        Ok(luned::Reached::Matched(daemon)) => Ok(daemon),
        Ok(luned::Reached::Unmatched(unmatched)) => Err(Unreached::Failed(unmatched.to_string())),
        Err(luned::Unreached::Busy) => Err(Unreached::Failed(luned::Unreached::Busy.to_string())),
        Err(luned::Unreached::Changed) => {
            if folder::held(&folder).map_err(refused)? {
                Err(Unreached::Failed(luned::Unreached::Changed.to_string()))
            } else {
                Err(Unreached::NoneRuns(path.to_owned()))
            }
        }
        Err(luned::Unreached::Connect(error)) => {
            if folder::held(&folder).map_err(refused)? {
                Err(Unreached::Failed(format!(
                    "{}: {}",
                    folder.socket().display(),
                    causes(&error)
                )))
            } else {
                Err(Unreached::NoneRuns(path.to_owned()))
            }
        }
        Err(luned::Unreached::Asked(status)) => Err(Unreached::Failed(causes(&status))),
    }
}

pub async fn info(path: &Path) -> Result<InfoResponse, Unreached> {
    let refused = |refusal: folder::Refusal| Unreached::Failed(refusal.to_string());
    let folder = folder::open(path).map_err(refused)?;
    let folder = folder.ok_or_else(|| Unreached::NoneRuns(path.to_owned()))?;
    match luned::info_of_any_build(&folder, luned::ANSWER).await {
        Ok(info) => Ok(info),
        Err(luned::Unreached::Connect(error)) => {
            if folder::held(&folder).map_err(refused)? {
                Err(Unreached::Failed(format!(
                    "{}: {}",
                    folder.socket().display(),
                    causes(&error)
                )))
            } else {
                Err(Unreached::NoneRuns(path.to_owned()))
            }
        }
        Err(error) => Err(Unreached::Failed(causes(&error))),
    }
}

pub async fn current(path: &Path) -> Result<Current, String> {
    match folder::open(path) {
        Ok(Some(folder)) => {
            if let Some(daemon) = luned::kept(&folder) {
                return Ok(Current { folder, daemon });
            }
        }
        Ok(None) => {}
        Err(refusal) => return Err(refusal.to_string()),
    }
    let (path, tasks) = (path.to_owned(), luned::tasks::shared());
    let reaching = tasks.clone();
    let reached = tasks.spawn_blocking(move || reaching.block_on(reach(&path)));
    reached.await.unwrap_or_else(|ended| {
        assert!(ended.is_panic(), "a daemon reach task is never cancelled");
        std::panic::resume_unwind(ended.into_panic())
    })
}

type ReadyWake = Pin<Box<dyn Future<Output = io::Result<ReadyLine>> + Send>>;

type ExitWake = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

enum ExitWait {
    Watching(ExitWake),
    Exited,
    Unavailable,
}

enum StopOutcome {
    Exited,
    Stopping(Stopped),
}

struct Stopped {
    pid: PeerPid,
    exit: ExitWait,
}

#[derive(Clone, Copy)]
enum Woke {
    Folder,
    Process,
    TimedOut,
}

enum Probe {
    Reached(Shared),
    Waiting { why: String, recheck: bool },
}

async fn probe(
    folder: &Folder,
    socket: &Path,
    stopped: &mut Option<Stopped>,
    doubted: &mut Option<PeerPid>,
    starting: &OnceCell<Option<Build>>,
) -> Result<Probe, String> {
    let why = match luned::reach(folder).await {
        Ok(luned::Reached::Matched(daemon)) => return Ok(Probe::Reached(daemon)),
        Ok(luned::Reached::Unmatched(unmatched)) => {
            let (other, pid) = (unmatched.other(), unmatched.pid());
            if let Some(pid) = pid
                && stopped.as_ref().is_some_and(|stopped| stopped.pid == pid)
            {
                format!("luned {other} (pid {pid}) was stopped, and has not ended")
            } else if !makes_way(other.build(), OWN, || *starting.get_or_init(build)) {
                return Err(kept(other, pid, starting.get().copied().flatten()));
            } else if let Some(pid) = pid {
                match listener(folder, pid).await {
                    Listener::Other if *doubted == Some(pid) => {
                        return Err(format!(
                            "luned {other} (pid {pid}) is older than this lune, {}, \
                             but the kernel says another process listens on its socket, \
                             so lune stops none",
                            *release::SAID
                        ));
                    }
                    Listener::Other => {
                        *doubted = Some(pid);
                        let why = format!("luned {other} (pid {pid}) is not the one that listens");
                        return Ok(Probe::Waiting { why, recheck: true });
                    }
                    Listener::Gone => {
                        let why = format!("luned {other} (pid {pid}) has ended since it answered");
                        return Ok(Probe::Waiting { why, recheck: true });
                    }
                    Listener::It => match stop(pid)? {
                        StopOutcome::Exited => {
                            let why =
                                format!("luned {other} (pid {pid}) has ended since it answered");
                            return Ok(Probe::Waiting { why, recheck: true });
                        }
                        StopOutcome::Stopping(stopping) => {
                            *stopped = Some(stopping);
                            let older = format!("luned {other} (pid {pid})");
                            format!("{older} is older than lune, and ends")
                        }
                    },
                }
            } else {
                return Err(format!(
                    "luned {other} is older than this lune, {}, but says no valid pid, so \
                     lune stops none",
                    *release::SAID
                ));
            }
        }
        Err(luned::Unreached::Busy) => return Err(luned::Unreached::Busy.to_string()),
        Err(luned::Unreached::Changed) => luned::Unreached::Changed.to_string(),
        Err(luned::Unreached::Asked(status)) => {
            format!("{}: {}", socket.display(), causes(&status))
        }
        Err(luned::Unreached::Connect(error)) => {
            format!("{}: {}", socket.display(), causes(&error))
        }
    };
    Ok(Probe::Waiting {
        why,
        recheck: false,
    })
}

async fn reach(path: &Path) -> Result<Current, String> {
    let folder = match folder::open(path).map_err(|refusal| refusal.to_string())? {
        Some(folder) => folder,
        None => folder::make(path).map_err(|refusal| refusal.to_string())?,
    };
    let socket = folder.socket();
    let deadline = Instant::now() + START;
    let mut started = Starting::NotYet;
    let mut stopped: Option<Stopped> = None;
    let mut doubted: Option<PeerPid> = None;
    let mut bind_retries = 0_u8;
    let mut spawns = 0_u8;
    let starting = OnceCell::new();
    let mut last_why = format!("{}: no answer yet", socket.display());
    let first = match probe(&folder, &socket, &mut stopped, &mut doubted, &starting).await? {
        Probe::Reached(daemon) => return Ok(Current { folder, daemon }),
        Probe::Waiting { why, recheck } => (why, recheck),
    };
    let mut watch = FolderWatch::on(folder.path(), FOLDER_NAMES);
    let mut initial = Some(first);
    for _ in 0..REACH_ATTEMPTS_MAX {
        let mark = watch.as_mut().map(FolderWatch::mark);
        let (why, recheck, from_initial) = match initial.take() {
            Some((why, recheck)) => (why, recheck, true),
            None => match probe(&folder, &socket, &mut stopped, &mut doubted, &starting).await {
                Ok(Probe::Reached(daemon)) => {
                    reap(started);
                    return Ok(Current { folder, daemon });
                }
                Ok(Probe::Waiting { why, recheck }) => (why, recheck, false),
                Err(why) => {
                    reap(started);
                    return Err(why);
                }
            },
        };
        last_why.clone_from(&why);
        let mut held = false;
        let mut try_now = false;
        match &mut started {
            Starting::NotYet => {
                held = folder::held(&folder).map_err(|refusal| refusal.to_string())?;
                if !held {
                    if spawns >= START_SPAWNS_MAX {
                        break;
                    }
                    spawns += 1;
                    started = Starting::Started(start(&folder)?);
                }
            }
            Starting::Started(daemon) => match daemon.child.try_wait() {
                Ok(None) => {}
                Ok(Some(ended)) if ended.code() == Some(ANOTHER_RUNS.into()) => {
                    started = Starting::NotYet;
                    try_now = true;
                }
                Ok(Some(ended)) if ended.signal() == Some(Signal::TERM.as_raw()) => {
                    started = Starting::Stopped;
                    try_now = true;
                }
                Ok(Some(ended)) => return Err(daemon.failed(ended)),
                Err(error) => return Err(format!("{LUNED}: {error}")),
            },
            Starting::Stopped => {}
        }
        if Instant::now() >= deadline {
            break;
        }
        if try_now || (from_initial && mark.is_some() && held) {
            continue;
        }
        let child_exited = matches!(
            &started,
            Starting::Started(daemon) if matches!(daemon.exit, ExitWait::Exited)
        );
        let child_exit_unavailable = matches!(
            &started,
            Starting::Started(daemon) if matches!(daemon.exit, ExitWait::Unavailable)
        );
        let stopped_exit_watching = matches!(
            &stopped,
            Some(Stopped {
                exit: ExitWait::Watching(_),
                ..
            })
        );
        let retry_bind = !recheck
            && !child_exited
            && !stopped_exit_watching
            && socket.exists()
            && bind_retries < BIND_RETRIES_MAX;
        if retry_bind {
            bind_retries += 1;
        }
        let use_bind_retry = recheck || child_exited || retry_bind;
        let use_hold_fallback =
            mark.is_none() || child_exit_unavailable || (held && !stopped_exit_watching);
        let until = if use_bind_retry {
            (Instant::now() + BIND_RETRY).min(deadline)
        } else if use_hold_fallback {
            (Instant::now() + HOLD_FALLBACK).min(deadline)
        } else {
            deadline
        };
        let woke = wait_for_event(&mut started, &mut stopped, mark, until).await;
        if matches!(woke, Woke::Folder) {
            bind_retries = 0;
        }
    }
    let secs = START.as_secs();
    let why = match &started {
        Starting::Started(daemon) => format!(
            "the luned lune started did not answer in {secs}s, and {} may say why: {last_why}",
            daemon.log.display()
        ),
        Starting::NotYet => format!("no luned answered in {secs}s: {last_why}"),
        Starting::Stopped => format!(
            "the luned lune started was stopped, and no other answered in {secs}s: {last_why}"
        ),
    };
    reap(started);
    Err(why)
}

fn log_ready_outcome(outcome: io::Result<ReadyLine>) {
    match outcome {
        Ok(ReadyLine::Matched | ReadyLine::Ended) => {}
        Ok(ReadyLine::Overflow) => {
            log::say(&format!("{LUNED} stdout passed its readiness byte limit"));
        }
        Err(error) => log::say(&format!("{LUNED} stdout readiness read failed: {error}")),
    }
}

async fn wait_for_event(
    started: &mut Starting,
    stopped: &mut Option<Stopped>,
    mark: Option<FolderMark<'_>>,
    until: Instant,
) -> Woke {
    let mut changed = pin!(async {
        match mark {
            Some(mark) => mark.changed().await,
            None => std::future::pending().await,
        }
    });
    let wait = std::future::poll_fn(|cx| {
        if let Starting::Started(daemon) = started {
            if let Some(ready) = daemon.ready.as_mut()
                && let Poll::Ready(outcome) = ready.as_mut().poll(cx)
            {
                daemon.ready = None;
                log_ready_outcome(outcome);
                return Poll::Ready(Woke::Process);
            }
            if let ExitWait::Watching(exit) = &mut daemon.exit
                && let Poll::Ready(outcome) = exit.as_mut().poll(cx)
            {
                daemon.exit = match outcome {
                    Ok(()) => ExitWait::Exited,
                    Err(error) => {
                        log::say(&format!("{LUNED} exit wait failed: {error}"));
                        ExitWait::Unavailable
                    }
                };
                return Poll::Ready(Woke::Process);
            }
        }
        if let Some(stopped) = stopped.as_mut()
            && let ExitWait::Watching(exit) = &mut stopped.exit
            && let Poll::Ready(outcome) = exit.as_mut().poll(cx)
        {
            let pid = stopped.pid;
            stopped.exit = match outcome {
                Ok(()) => ExitWait::Exited,
                Err(error) => {
                    log::say(&format!("{LUNED} (pid {pid}) exit wait failed: {error}"));
                    ExitWait::Unavailable
                }
            };
            return Poll::Ready(Woke::Process);
        }
        if changed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Woke::Folder);
        }
        Poll::Pending
    });
    tokio::time::timeout_at(until.into(), wait)
        .await
        .unwrap_or(Woke::TimedOut)
}

pub fn unmatched(other: &Other, pid: Option<PeerPid>) -> String {
    luned::Unmatched::of(other.clone(), pid).to_string()
}

fn kept(other: &Other, pid: Option<PeerPid>, starting: Option<Build>) -> String {
    if other.is_newer() {
        let unmatched = unmatched(other, pid);
        let end = pid.map_or_else(
            || "end it".to_owned(),
            |pid| format!("end it (`kill {pid}`)"),
        );
        return format!("{unmatched}, or {end} for this one to start its own");
    }
    let (own, program) = (release::SAID.as_str(), luned());
    let pid = pid.map_or_default(|pid| format!(" (pid {pid})"));
    let says = starting.map_or_else(|| "says no build".to_owned(), |it| format!("is {it}"));
    format!(
        "luned {other}{pid} is another build than this lune, {own}, but {} {says}, \
         no newer, so lune leaves it be",
        program.display()
    )
}

fn reap(started: Starting) {
    if let Starting::Started(Started { mut child, .. }) = started {
        let _ = thread::Builder::new()
            .name(LUNED.to_owned())
            .spawn(move || child.wait());
    }
}

enum Starting {
    NotYet,
    Started(Started),
    Stopped,
}

struct Started {
    child: Child,
    log: PathBuf,
    from: u64,
    ready: Option<ReadyWake>,
    exit: ExitWait,
}

impl Started {
    fn failed(&self, ended: ExitStatus) -> String {
        let said = self.said().unwrap_or_default();
        match said.lines().map(str::trim).find(|line| !line.is_empty()) {
            Some(line) => format!("{LUNED} did not start: {line}"),
            None => format!(
                "{LUNED} did not start ({ended}), and wrote nothing in {}",
                self.log.display()
            ),
        }
    }

    fn said(&self) -> io::Result<String> {
        let mut log = File::open(&self.log)?;
        log.seek(SeekFrom::Start(self.from))?;
        let mut said = Vec::new();
        log.take(SAID_MAX).read_to_end(&mut said)?;
        Ok(String::from_utf8_lossy(&said).into_owned())
    }
}

fn start(folder: &Folder) -> Result<Started, String> {
    let path = folder.path().join(LOG);
    let opened = || {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        let mut from = log.metadata()?.len();
        if from > LOG_MAX {
            log.set_len(0)?;
            from = 0;
        }
        io::Result::Ok((log, from))
    };
    let (log, from) = opened().map_err(|error| format!("{}: {error}", path.display()))?;
    let program = luned();
    let mut child = Command::new(&program)
        .arg("--detached")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .map_err(|error| format!("{} could not be started: {error}", program.display()))?;
    let stdout = child.stdout.take().expect("its stdout is piped");
    let ready: Option<ReadyWake> = Some(Box::pin(luned::read_ready_line(stdout, LISTENING)));
    let exit = match ProcessExit::watch(Pid::from_child(&child)) {
        Ok(Some(exit)) => ExitWait::Watching(Box::pin(exit.wait())),
        Ok(None) => ExitWait::Exited,
        Err(error) => {
            log::say(&format!("{LUNED} exit watch failed: {error}"));
            ExitWait::Unavailable
        }
    };
    Ok(Started {
        child,
        log: path,
        from,
        ready,
        exit,
    })
}

fn luned() -> PathBuf {
    program::luned().unwrap_or_else(|_| PathBuf::from(LUNED))
}

fn makes_way(running: Option<Build>, own: Build, starting: impl FnOnce() -> Option<Build>) -> bool {
    let older = |than: Build| running.is_none_or(|running| running < than);
    older(own) && starting().is_some_and(older)
}

fn build() -> Option<Build> {
    let mut luned = Command::new(luned());
    luned.arg("--version");
    let said = program::run(&mut luned, VERSION_LIMIT)
        .ok()?
        .answer()
        .ok()?;
    let said = String::from_utf8_lossy(&said);
    Build::parse(said.trim().strip_prefix(LUNED)?.strip_prefix(' ')?)
}

enum Listener {
    It,
    Other,
    Gone,
}

async fn listener(folder: &Folder, pid: PeerPid) -> Listener {
    match luned::listening(folder).await {
        Ok(listening) if listening == Some(pid) => Listener::It,
        Ok(_) => Listener::Other,
        Err(_) => Listener::Gone,
    }
}

fn stop(pid: PeerPid) -> Result<StopOutcome, String> {
    match ProcessExit::watch(pid.as_pid()) {
        Ok(Some(exit)) => match exit.signal(Signal::TERM) {
            Ok(()) => Ok(StopOutcome::Stopping(Stopped {
                pid,
                exit: ExitWait::Watching(Box::pin(exit.wait())),
            })),
            Err(rustix::io::Errno::SRCH) => Ok(StopOutcome::Exited),
            Err(error) => Err(format!("{LUNED} (pid {pid}) could not be stopped: {error}")),
        },
        Ok(None) => Ok(StopOutcome::Exited),
        Err(error) => {
            log::say(&format!("{LUNED} (pid {pid}) exit watch failed: {error}"));
            match rustix::process::kill_process(pid.as_pid(), Signal::TERM) {
                Ok(()) => Ok(StopOutcome::Stopping(Stopped {
                    pid,
                    exit: ExitWait::Unavailable,
                })),
                Err(rustix::io::Errno::SRCH) => Ok(StopOutcome::Exited),
                Err(error) => Err(format!("{LUNED} (pid {pid}) could not be stopped: {error}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_makes_way_for_a_newer_build_alone() {
        let way = |running, own, starting| {
            makes_way(Build::parse(running), Build::parse(own).unwrap(), || {
                Build::parse(starting)
            })
        };
        assert!(way("0.2.2", "0.2.2+2", "0.2.2+2"));
        assert!(way("0.2.2+1", "0.2.2+2", "0.2.2+2"));
        assert!(way("0.2.1+3", "0.2.2+2", "0.2.2+2"));
        assert!(way("0.0", "0.2.2", "0.2.2"));
        assert!(!way("0.2.2+2", "0.2.2+2", "0.2.2+2"));
        assert!(!way("0.2.2+2", "0.2.2", "0.2.2"));
        assert!(!way("0.2.3+1", "0.2.2+2", "0.2.2+2"));
        assert!(!way("0.2.2+1", "0.2.2+2", "0.2.2+1"));
        assert!(!way("0.2.2+1", "0.2.2+2", "0.0"));
    }

    #[test]
    fn a_task_on_lune_s_tasks_reaches_luned_as_a_thread_does() {
        use std::os::unix::fs::PermissionsExt as _;

        let open = tempfile::tempdir().unwrap();
        std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = open.path().to_owned();
        let tasks = luned::tasks::shared();
        let reaching = tasks.spawn(async move { current(&path).await.map(|_| ()) });
        let reached = tasks.block_on(reaching).unwrap();
        let refused = folder::Refusal::Open(open.path().to_owned()).to_string();
        assert_eq!(
            reached,
            Err(refused),
            "an open folder is refused without blocking"
        );

        let blocked = tempfile::tempdir().unwrap();
        std::fs::set_permissions(blocked.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let missing = blocked.path().join("lune");
        let path = missing.clone();
        let reaching = tasks.spawn(async move { current(&path).await.map(|_| ()) });
        let reached = tasks.block_on(reaching).unwrap();
        std::fs::set_permissions(blocked.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let Err(why) = reached else {
            panic!("a missing folder in a read-only parent was made");
        };
        assert!(
            why.contains(&missing.display().to_string()),
            "a worker hands the reach to a thread that may block: {why}"
        );
    }

    #[test]
    fn a_newer_daemon_with_no_valid_pid_suggests_no_kill_command() {
        let release::Of::Other(newer) = release::of("99.0.0", None) else {
            panic!("99.0.0 is another build");
        };
        let without_pid = kept(&newer, None, None);
        assert!(!without_pid.contains("kill "), "{without_pid}");
        let with_pid = kept(&newer, PeerPid::new(4242), None);
        assert!(with_pid.contains("`kill 4242`"), "{with_pid}");
    }
}
