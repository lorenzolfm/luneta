//! Another program the daemon asks something of: zellij what panes a session
//! has, git how a work tree stands, gh what pull request a branch has; or that
//! luneta does: lunetad its version, wt a worktree of a pull request.
//! Each answers on its stdout, and has a time to answer in: one that takes
//! longer is stuck, and is killed, as one is whose answer is no longer wanted.

use std::convert::Infallible;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions};

/// How often whether a program is still wanted is asked, while it runs, where
/// it is wanted only for a while; and whether one has ended, where what was
/// said of it is no end, as macOS says of one that stopped.
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
    let Ok(ran) = running::<Infallible>(command, limit, None, Kills::Program)?;
    Ok(ran)
}

/// Runs `command` as `run` does, in a session of its own, with no terminal,
/// and kills what it started with it, where it is stuck. A program that asks
/// across the network, as git does by ssh, may want a passphrase or a
/// password, which it asks for on the terminal of its session, whatever its
/// stdin: on luneta's, it would draw over luneta's screen, and read the keys
/// typed there. With none, it fails, and says why. What it started, as git
/// does that wt runs, would run on once it is killed.
pub fn run_alone(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    let Ok(ran) = running::<Infallible>(alone(command), limit, None, Kills::Group)?;
    Ok(ran)
}

/// Runs `command` as `run` does, for as long as `wanted` says what it would
/// say is still wanted, which it is asked each `LOOK` until it ends: none
/// once it is not, as it is killed and waited for then, as one stuck is.
pub fn run_while(
    command: &mut Command,
    limit: Duration,
    wanted: impl Fn() -> bool,
) -> io::Result<Option<Ran>> {
    let wanted = || if wanted() { Ok(()) } else { Err(()) };
    running::<()>(command, limit, Some(&wanted), Kills::Program).map(Result::ok)
}

/// What of a program is killed, as it is stuck or no longer wanted.
#[derive(Clone, Copy)]
enum Kills {
    /// The program alone.
    Program,
    /// The program, and what it started: the group it leads, as `alone` has
    /// it lead one.
    Group,
}

/// Runs `command`, as `run_while` does, where it is `wanted` only for a
/// while, which says what that comes to once it is not, or else as `run`
/// does, and kills of it what `kills` says.
fn running<N>(
    command: &mut Command,
    limit: Duration,
    wanted: Option<&dyn Fn() -> Result<(), N>>,
    kills: Kills,
) -> io::Result<Result<Ran, N>> {
    let deadline = Instant::now() + limit;
    let mut child =
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let heard = hear(&mut child, deadline, wanted);
    // Whatever was heard of it, or why nothing could be, what is left of it
    // is killed, and it is waited for last, so that no kill names a pid let
    // go. One that could not be killed is let go unwaited for, rather than
    // waited for with no deadline.
    if !matches!(heard, Ok(Heard::Ended { .. })) {
        kill(&mut child, kills)?;
    }
    let status = child.wait()?;
    Ok(match heard? {
        Heard::Ended { stdout, stderr } => Ok(Ran::Ended { status, stdout, stderr }),
        Heard::Stuck => Ok(Ran::Stuck),
        Heard::Unwanted(not) => Err(not),
    })
}

/// What was heard of a program, before it is waited for.
enum Heard<N> {
    /// It ended, and said all it had to say in its time.
    Ended { stdout: Vec<u8>, stderr: Vec<u8> },
    /// It, or what it started that holds its stdout open, went past its
    /// deadline.
    Stuck,
    /// It was no longer wanted, and this is what that comes to.
    Unwanted(N),
}

/// What is heard of `child` until `deadline`, for as long as it is `wanted`.
fn hear<N>(
    child: &mut Child,
    deadline: Instant,
    wanted: Option<&dyn Fn() -> Result<(), N>>,
) -> io::Result<Heard<N>> {
    let stdout = read(child.stdout.take().expect("its stdout is piped"))?;
    let stderr = read(child.stderr.take().expect("its stderr is piped"))?;
    let ended = ends(child)?;
    let left = || deadline.saturating_duration_since(Instant::now());
    loop {
        if Instant::now() >= deadline {
            return Ok(Heard::Stuck);
        }
        if let Some(Err(not)) = wanted.map(|wanted| wanted()) {
            return Ok(Heard::Unwanted(not));
        }
        // Heard from as it ends, or at its deadline, and where it is wanted
        // only for a while, each `LOOK` besides, to ask whether it still is.
        let look = if wanted.is_some() { left().min(LOOK) } else { left() };
        match ended.recv_timeout(look) {
            Ok(end) => break end?,
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the thread that waits for a program says how that went")
            },
        }
    }
    let (Ok(stdout), Ok(stderr)) = (stdout.recv_timeout(left()), stderr.recv_timeout(left()))
    else {
        // What it started holds its stdout open still.
        return Ok(Heard::Stuck);
    };
    Ok(Heard::Ended { stdout: stdout?, stderr: stderr? })
}

/// Kills of `child` what `kills` says. One that ended, and is not waited for
/// yet, takes the signal all the same, and so does the group it leads.
fn kill(child: &mut Child, kills: Kills) -> io::Result<()> {
    match kills {
        Kills::Program => child.kill(),
        Kills::Group => kill_group(child),
    }
}

/// Has `command` lead a session of its own, with no terminal, and a group of
/// its own in it, numbered as it is.
#[expect(unsafe_code, reason = "std has no safe way to start a command in a session of its own")]
fn alone(command: &mut Command) -> &mut Command {
    // SAFETY: the closure runs in the child, between its fork and its exec,
    // where only what a signal handler may do is safe: `setsid` is a system
    // call alone, and its error holds no memory.
    unsafe {
        command.pre_exec(|| process::setsid().map(|_| ()).map_err(io::Error::from));
    }
    command
}

/// Kills the group `child` leads, which ends what it started, as the group
/// of its session. One that has all ended is no failure.
fn kill_group(child: &Child) -> io::Result<()> {
    match process::kill_process_group(Pid::from_child(child), Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Says once `child` has ended, from a thread of its own, or why that could
/// not be told. It is left for `child` to be waited for: until then, its pid
/// is not let go, and the kill at its deadline can name no other process.
fn ends(child: &Child) -> io::Result<Receiver<io::Result<()>>> {
    let pid = Pid::from_child(child);
    let ended = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    in_thread(move || {
        loop {
            match process::waitid(WaitId::Pid(pid), ended) {
                Ok(Some(status)) if status.exited() || status.killed() || status.dumped() => {
                    return Ok(());
                },
                Err(Errno::INTR) => {},
                Err(error) => return Err(error.into()),
                // What is no end, as macOS says of one that stopped, each
                // time it is asked until it goes on, where Linux waits for
                // its end: it is asked again each `LOOK` meanwhile, until it
                // ends or is killed at its deadline.
                Ok(_) => thread::sleep(LOOK),
            }
        }
    })
}

/// What comes out of `pipe` until it closes, read in a thread of its own and
/// handed over in one piece, or why it could not all be read.
fn read(mut pipe: impl Read + Send + 'static) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
    in_thread(move || {
        let mut all = Vec::new();
        let read = (&mut pipe).take(SAID_MAX).read_to_end(&mut all);
        // What it says past the most is read and let go, so that it is not
        // kept waiting.
        let past = read.and_then(|_| io::copy(&mut pipe, &mut io::sink()));
        match past {
            Ok(0) => Ok(all),
            Ok(_) => Err(io::Error::other(format!("it said more than {SAID_MAX} bytes"))),
            Err(error) => Err(error),
        }
    })
}

/// What `work` comes to, worked out in a thread of its own and handed over
/// once it is done.
fn in_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> io::Result<Receiver<T>> {
    let (said, heard) = mpsc::sync_channel(1);
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        // One that stopped waiting has no use for it.
        let _ = said.send(work());
    })?;
    Ok(heard)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use rustix::process::{self, Pid, Signal};

    use super::{Ran, SAID_MAX, alone, run, run_alone, run_while};

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

    /// A program is heard from as it ends, about as soon as std's own `output`
    /// hears from it, which waits for its pipes and its end with no timer: no
    /// look each few milliseconds holds its answer up.
    #[test]
    fn a_program_is_heard_from_as_it_ends() {
        // A busy machine holds a round up, and never hurries one, so the
        // quickest quarter says the most of how soon it is heard from.
        let quick = |mut took: Vec<Duration>| {
            took.sort();
            took[took.len() / 4]
        };
        let (mut ran, mut output) = (Vec::new(), Vec::new());
        // Turn about, so that a machine busy for a while slows both alike.
        for _ in 0..21 {
            let asked = Instant::now();
            let said = run(&mut Command::new("true"), Duration::from_secs(10)).unwrap().answer();
            ran.push(asked.elapsed());
            assert_eq!(said, Ok(Vec::new()));
            let asked = Instant::now();
            assert!(Command::new("true").output().unwrap().status.success());
            output.push(asked.elapsed());
        }
        let (ran, output) = (quick(ran), quick(output));
        assert!(ran < output + Duration::from_millis(2), "{ran:?} against {output:?}");
    }

    /// A program a signal kills, rather than its deadline, is heard from as
    /// it ends, as one is that exits.
    #[test]
    fn a_program_a_signal_kills_is_heard_from_as_it_ends() {
        let asked = Instant::now();
        let mut dies = Command::new("sh");
        dies.args(["-c", "kill -KILL $$"]);
        let Ran::Ended { status, .. } = run(&mut dies, Duration::from_secs(10)).unwrap() else {
            panic!("it was not heard to end");
        };
        assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
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

    /// A program that stopped is stuck once its time is up, and killed, as
    /// one that runs on is: macOS says it ended, though it can still go on.
    #[test]
    fn a_program_that_stopped_is_stuck() {
        let asked = Instant::now();
        let mut stops = Command::new("sh");
        stops.args(["-c", "kill -STOP $$"]);
        let ran = run(&mut stops, Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }

    /// A program run alone leads a session of its own, with no terminal to
    /// ask for a passphrase on, rather than its caller's.
    #[test]
    fn a_program_run_alone_has_a_session_of_its_own() {
        let mut sleep = Command::new("sleep");
        let mut child = alone(sleep.arg("10")).spawn().expect("sleep runs");
        let pid = Pid::from_child(&child);
        let session = process::getsid(Some(pid));
        child.kill().expect("sleep is killed");
        child.wait().expect("sleep ends");
        assert_eq!(session, Ok(pid));
    }

    /// A program run alone that is stuck is killed with what it started, which
    /// would run on otherwise, as git does that wt runs: one that does not end
    /// in its time, and one that ended, where what it started holds its stdout
    /// open past then.
    #[test]
    fn a_program_run_alone_that_is_stuck_is_killed_with_what_it_started() {
        for script in [r#"sleep 30 & echo $! > "$0"; wait"#, r#"sleep 30 & echo $! > "$0""#] {
            let dir = tempfile::tempdir().expect("a folder");
            let started = dir.path().join("started");
            let mut sh = Command::new("sh");
            sh.args(["-c", script]).arg(&started);
            let ran = run_alone(&mut sh, Duration::from_millis(500)).expect("sh runs");
            assert!(matches!(ran, Ran::Stuck), "{script}");
            let pid = fs::read_to_string(&started).expect("sh says what it started");
            let pid = Pid::from_raw(pid.trim().parse().expect("a pid")).expect("a pid");
            // Gone once the process that took it in has waited for it.
            let gone = Instant::now() + Duration::from_secs(5);
            while process::test_kill_process(pid).is_ok() {
                assert!(Instant::now() < gone, "what `{script}` started runs on");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
