//! Another program the daemon asks something of: zellij what panes a session
//! has, git how a work tree stands, gh what pull request a branch has; or that
//! luneta does: lunetad its version.
//! Each answers on its stdout, and has a time to answer in: one that takes
//! longer is stuck, and is killed, as one is whose answer is no longer wanted.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// How often a program that runs is looked at, to see whether it has ended.
const LOOK: Duration = Duration::from_millis(5);

/// The most of what a program says that the daemon reads. `git status` says a
/// line for each path with changes, and one that says more than this many
/// bytes of them has failed: counted short, it would tell a work tree wrong.
pub const SAID_MAX: u64 = 16 * 1024 * 1024;

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
/// longer than that. An error when it could not be started, or what it said
/// could not all be read, as when it said more than the most.
pub fn run(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    let ran = run_while(command, limit, || true)?;
    Ok(ran.expect("a program wanted all along is never given up"))
}

/// Runs `command` as `run` does, for as long as `wanted` says what it would
/// say is still wanted, which it is asked each `LOOK` until it ends: none
/// once it is not, as it is killed and waited for then, as one stuck is.
pub fn run_while(
    command: &mut Command,
    limit: Duration,
    wanted: impl Fn() -> bool,
) -> io::Result<Option<Ran>> {
    let deadline = Instant::now() + limit;
    let mut child =
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = read(child.stdout.take().expect("its stdout is piped"))?;
    let stderr = read(child.stderr.take().expect("its stderr is piped"))?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let stuck = Instant::now() >= deadline;
        if stuck || !wanted() {
            // One that ended just now is not waited for yet, and takes the
            // signal all the same.
            child.kill()?;
            child.wait()?;
            return Ok(stuck.then_some(Ran::Stuck));
        }
        thread::sleep(LOOK);
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    let (Ok(stdout), Ok(stderr)) = (stdout.recv_timeout(left()), stderr.recv_timeout(left()))
    else {
        return Ok(Some(Ran::Stuck));
    };
    Ok(Some(Ran::Ended { status, stdout: stdout?, stderr: stderr? }))
}

/// What comes out of `pipe` until it closes, read in a thread of its own and
/// handed over in one piece, or why it could not all be read.
fn read(mut pipe: impl Read + Send + 'static) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
    let (said, heard) = mpsc::sync_channel(1);
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        let mut all = Vec::new();
        let read = (&mut pipe).take(SAID_MAX).read_to_end(&mut all);
        // What it says past the most is read and let go, so that it is not
        // kept waiting.
        let past = read.and_then(|_| io::copy(&mut pipe, &mut io::sink()));
        let all = match past {
            Ok(0) => Ok(all),
            Ok(_) => Err(io::Error::other(format!("it said more than {SAID_MAX} bytes"))),
            Err(error) => Err(error),
        };
        // One that stopped waiting has no use for it.
        let _ = said.send(all);
    })?;
    Ok(heard)
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{Ran, SAID_MAX, run, run_while};

    /// A program that says more than the most has failed, rather than been
    /// read short.
    #[test]
    fn a_program_that_says_too_much_has_failed() {
        let mut says = Command::new("head");
        says.args(["-c", &(SAID_MAX + 1).to_string(), "/dev/zero"]);
        let Err(error) = run(&mut says, Duration::from_secs(10)) else {
            panic!("it said more than the most, and was read");
        };
        assert!(error.to_string().contains("said more than"), "{error}");
    }

    /// A program that does not end in its time is killed and waited for, so
    /// that it holds the daemon up no longer than that.
    #[test]
    fn a_program_that_does_not_end_in_time_is_stuck() {
        let asked = Instant::now();
        let ran = run(Command::new("sleep").arg("10"), Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }

    /// A program whose answer is no longer wanted is killed and waited for
    /// then, long before its time is up, and says nothing.
    #[test]
    fn a_program_no_longer_wanted_is_killed() {
        let asked = Instant::now();
        let wanted = || asked.elapsed() < Duration::from_millis(200);
        let ran = run_while(Command::new("sleep").arg("10"), Duration::from_secs(10), wanted);
        assert!(ran.unwrap().is_none());
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }
}
