//! Another program the daemon asks something of: zellij what panes a session
//! has, git how a work tree stands. Each answers on its stdout, and has a time
//! to answer in: one that takes longer is stuck, and is killed.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// How often a program that runs is looked at, to see whether it has ended.
const LOOK: Duration = Duration::from_millis(5);

/// The most of what a program says that the daemon reads. `git status` says a
/// line for each path with changes, and a work tree with more than this many
/// bytes of them is counted short.
const SAID_MAX: u64 = 16 * 1024 * 1024;

/// How a program that was asked something ended.
pub enum Ran {
    /// It ended on its own, and said `stdout` and `stderr`.
    Ended { status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8> },
    /// It had not said all it had to say within its time, and was killed.
    Stuck,
}

impl Ran {
    /// What it said on stdout, if it ended with success, or else why not: the
    /// first line of what it said on stderr, or how it ended.
    pub fn answer(self) -> Result<Vec<u8>, String> {
        match self {
            Ran::Ended { status, stdout, .. } if status.success() => Ok(stdout),
            Ran::Ended { status, stderr, .. } => {
                let said = String::from_utf8_lossy(&stderr);
                match said.lines().map(str::trim).find(|line| !line.is_empty()) {
                    Some(line) => Err(line.to_owned()),
                    None => Err(status.to_string()),
                }
            },
            Ran::Stuck => Err("it did not answer in time".to_owned()),
        }
    }
}

/// Runs `command` with nothing on its stdin, for no longer than `limit`. What
/// it says is read as it says it, so that one that says a lot never waits on
/// the daemon to read it. The time covers what it says too: a process it
/// started, that holds its stdout open once it has ended, holds it up no
/// longer than that.
pub fn run(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    let deadline = Instant::now() + limit;
    let mut child =
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = read(child.stdout.take().expect("its stdout is piped"))?;
    let stderr = read(child.stderr.take().expect("its stderr is piped"))?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // It ends either way: one that ended just now has nothing left to
            // kill, and is waited for all the same.
            let _ = child.kill();
            child.wait()?;
            return Ok(Ran::Stuck);
        }
        thread::sleep(LOOK);
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    let (Ok(stdout), Ok(stderr)) = (stdout.recv_timeout(left()), stderr.recv_timeout(left()))
    else {
        return Ok(Ran::Stuck);
    };
    Ok(Ran::Ended { status, stdout, stderr })
}

/// What comes out of `pipe` until it closes, read in a thread of its own and
/// handed over in one piece.
fn read(mut pipe: impl Read + Send + 'static) -> io::Result<Receiver<Vec<u8>>> {
    let (said, heard) = mpsc::sync_channel(1);
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        let mut all = Vec::new();
        // What could not be read is what the program never said. What it says
        // past the most is read and let go, so that it is not kept waiting.
        let _ = (&mut pipe).take(SAID_MAX).read_to_end(&mut all);
        let _ = io::copy(&mut pipe, &mut io::sink());
        // One that stopped waiting has no use for it.
        let _ = said.send(all);
    })?;
    Ok(heard)
}
