use std::cell::OnceCell;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use luned::api::InfoResponse;
use luned::folder::{self, Folder, ANOTHER_RUNS, LOG};
use luned::{program, Daemon};
use luned::release::{self, Build, Of, Other, OWN};
use luned::terminate::LUNED;
use rustix::process::{Pid, Signal};

use crate::commands::causes;

pub const START: Duration = Duration::from_secs(5);

pub const LOOK: Duration = Duration::from_millis(20);

const LOG_MAX: u64 = 1024 * 1024;

const SAID_MAX: u64 = 4096;

pub const VERSION_LIMIT: Duration = Duration::from_secs(2);

pub const NONE_RUNS: u8 = 5;

pub struct Current {
    pub folder: Folder,
    pub daemon: Daemon,
    pub info: InfoResponse,
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

pub async fn running(path: &Path) -> Result<Daemon, Unreached> {
    let mut daemon = of_any_build(path).await?;
    let info = daemon.info(()).await;
    let info = info.map_err(|status| Unreached::Failed(causes(&status)))?.into_inner();
    match release::of(&info.version, info.built) {
        Of::Own => Ok(daemon),
        Of::Other(other) => Err(Unreached::Failed(unmatched(&other, info.pid))),
    }
}

pub async fn of_any_build(path: &Path) -> Result<Daemon, Unreached> {
    let refused = |refusal: folder::Refusal| Unreached::Failed(refusal.to_string());
    let folder = folder::open(path).map_err(refused)?;
    let folder = folder.ok_or_else(|| Unreached::NoneRuns(path.to_owned()))?;
    let error = match luned::connect(&folder, luned::ANSWER).await {
        Ok(daemon) => return Ok(daemon),
        Err(error) => error,
    };
    if folder::held(&folder).map_err(refused)? {
        Err(Unreached::Failed(format!("{}: {}", folder.socket().display(), causes(&error))))
    } else {
        Err(Unreached::NoneRuns(path.to_owned()))
    }
}

pub async fn current(path: &Path) -> Result<Current, String> {
    let folder = match folder::open(path).map_err(|refusal| refusal.to_string())? {
        Some(folder) => folder,
        None => folder::make(path).map_err(|refusal| refusal.to_string())?,
    };
    let socket = folder.socket();
    let deadline = Instant::now() + START;
    let mut started = Starting::NotYet;
    let mut stopped: Option<u32> = None;
    let mut doubted: Option<u32> = None;
    let starting = OnceCell::new();
    loop {
        let why = match luned::connect(&folder, luned::ANSWER).await {
            Ok(mut daemon) => match daemon.info(()).await {
                Ok(info) => {
                    let info = info.into_inner();
                    let of = release::of(&info.version, info.built);
                    let (version, pid) = (release::spelled(&info.version, info.built), info.pid);
                    if stopped == Some(pid) {
                        format!("luned {version} (pid {pid}) was stopped, and has not ended")
                    } else if of == Of::Own {
                        reap(started);
                        return Ok(Current { folder, daemon, info });
                    } else if let Of::Other(other) = &of
                        && !makes_way(other.build(), OWN, || *starting.get_or_init(build))
                    {
                        reap(started);
                        return Err(kept(other, pid, starting.get().copied().flatten()));
                    } else {
                        match listener(&folder, pid).await {
                            Listener::Other if doubted == Some(pid) => {
                                reap(started);
                                return Err(format!(
                                    "luned {version} (pid {pid}) is older than this lune, {}, \
                                     but the kernel says another process listens on its socket, \
                                     so lune stops none",
                                    *release::SAID
                                ));
                            },
                            Listener::Other => {
                                doubted = Some(pid);
                                format!("luned {version} (pid {pid}) is not the one that listens")
                            },
                            Listener::Gone => {
                                format!("luned {version} (pid {pid}) has ended since it answered")
                            },
                            Listener::It => {
                                stop(pid)?;
                                stopped = Some(pid);
                                let older = format!("luned {version} (pid {pid})");
                                format!("{older} is older than lune, and ends")
                            },
                        }
                    }
                },
                Err(status) => format!("{}: {}", socket.display(), causes(&status)),
            },
            Err(error) => format!("{}: {}", socket.display(), causes(&error)),
        };
        match &mut started {
            Starting::NotYet => {
                if !folder::held(&folder).map_err(|refusal| refusal.to_string())? {
                    started = Starting::Started(start(&folder)?);
                }
            },
            Starting::Started(daemon) => match daemon.child.try_wait() {
                Ok(None) => {},
                Ok(Some(ended)) if ended.code() == Some(ANOTHER_RUNS.into()) => {
                    started = Starting::NotYet;
                },
                Ok(Some(ended)) if ended.signal() == Some(Signal::TERM.as_raw()) => {
                    started = Starting::Stopped;
                },
                Ok(Some(ended)) => return Err(daemon.failed(ended)),
                Err(error) => return Err(format!("{LUNED}: {error}")),
            },
            Starting::Stopped => {},
        }
        if Instant::now() >= deadline {
            let secs = START.as_secs();
            let why = match &started {
                Starting::Started(daemon) => format!(
                    "the luned lune started did not answer in {secs}s, and {} may say why: \
                     {why}",
                    daemon.log.display()
                ),
                Starting::NotYet => format!("no luned answered in {secs}s: {why}"),
                Starting::Stopped => format!(
                    "the luned lune started was stopped, and no other answered in {secs}s: \
                     {why}"
                ),
            };
            reap(started);
            return Err(why);
        }
        tokio::time::sleep(LOOK).await;
    }
}

pub fn unmatched(other: &Other, pid: u32) -> String {
    let (daemon, own) = (format!("luned {other} (pid {pid})"), release::SAID.as_str());
    if other.is_newer() {
        format!("{daemon} is newer than this lune, {own}: run a lune of its build")
    } else {
        let replaced = "open lune to have it replaced";
        format!("{daemon} is another build than this lune, {own}: {replaced}")
    }
}

fn kept(other: &Other, pid: u32, starting: Option<Build>) -> String {
    if other.is_newer() {
        let unmatched = unmatched(other, pid);
        return format!("{unmatched}, or end it (`kill {pid}`) for this one to start its own");
    }
    let (own, program) = (release::SAID.as_str(), luned());
    let says = starting.map_or_else(|| "says no build".to_owned(), |it| format!("is {it}"));
    format!(
        "luned {other} (pid {pid}) is another build than this lune, {own}, but {} {says}, \
         no newer, so lune leaves it be",
        program.display()
    )
}

fn reap(started: Starting) {
    if let Starting::Started(Started { mut child, .. }) = started {
        let _ = thread::Builder::new().name(LUNED.to_owned()).spawn(move || child.wait());
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
        let log = OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
        let mut from = log.metadata()?.len();
        if from > LOG_MAX {
            log.set_len(0)?;
            from = 0;
        }
        io::Result::Ok((log, from))
    };
    let (log, from) = opened().map_err(|error| format!("{}: {error}", path.display()))?;
    let program = luned();
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
    let said = program::run(&mut luned, VERSION_LIMIT).ok()?.answer().ok()?;
    let said = String::from_utf8_lossy(&said);
    Build::parse(said.trim().strip_prefix(LUNED)?.strip_prefix(' ')?)
}

enum Listener {
    It,
    Other,
    Gone,
}

async fn listener(folder: &Folder, pid: u32) -> Listener {
    match luned::listening(folder).await {
        Ok(listening) if listening == Some(pid) => Listener::It,
        Ok(_) => Listener::Other,
        Err(_) => Listener::Gone,
    }
}

fn stop(pid: u32) -> Result<(), String> {
    let id = i32::try_from(pid).ok().and_then(Pid::from_raw);
    let id = id.ok_or_else(|| format!("{LUNED} says its pid is {pid}, which none is"))?;
    match rustix::process::kill_process(id, Signal::TERM) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(format!("{LUNED} (pid {pid}) could not be stopped: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_makes_way_for_a_newer_build_alone() {
        let way = |running, own, starting| {
            makes_way(Build::parse(running), Build::parse(own).unwrap(), || Build::parse(starting))
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
}
