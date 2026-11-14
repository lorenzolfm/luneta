use std::convert::Infallible;
use std::io::{self, Read, Write as _};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus};

const LOOK: Duration = Duration::from_millis(5);

pub const SAID_MAX: u64 = 16 * 1024 * 1024;

pub enum Ran {
    Ended { status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8> },
    Stuck,
}

impl Ran {
    pub fn answer(self) -> Result<Vec<u8>, String> {
        match self {
            Ran::Ended { status, stdout, .. } if status.success() => Ok(stdout),
            Ran::Ended { status, stderr, .. } => {
                Err(first_line(&stderr).unwrap_or_else(|| status.to_string()))
            },
            Ran::Stuck => Err("it did not answer in time".to_owned()),
        }
    }

    pub fn answer_why_on_either(self) -> Result<Vec<u8>, String> {
        match self {
            Ran::Ended { status, stdout, stderr } if !status.success() => {
                let why = first_line(&stdout).or_else(|| first_line(&stderr));
                Err(why.unwrap_or_else(|| status.to_string()))
            },
            ran => ran.answer(),
        }
    }
}

fn first_line(said: &[u8]) -> Option<String> {
    let said = String::from_utf8_lossy(said);
    said.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_owned)
}

pub fn run(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    run_up_to(command, limit, SAID_MAX)
}

pub fn run_reading(command: &mut Command, limit: Duration, stdin: Stdio) -> io::Result<Ran> {
    let kills = Kills::Program;
    let Ok((ran, _)) = running::<Infallible>(command, limit, None, kills, stdin, FIRST)?;
    Ok(ran)
}

pub fn saying(words: &str) -> io::Result<Stdio> {
    let (stdin, mut says) = io::pipe()?;
    let words = words.to_owned();
    in_thread(move || {
        let _ = says.write_all(words.as_bytes());
    })?;
    Ok(Stdio::from(stdin))
}

pub fn run_up_to(command: &mut Command, limit: Duration, most: u64) -> io::Result<Ran> {
    let kills = Kills::Program;
    let keeps = Keeps::First { most, over: io::ErrorKind::FileTooLarge };
    let Ok((ran, _)) = running::<Infallible>(command, limit, None, kills, Stdio::null(), keeps)?;
    Ok(ran)
}

pub fn run_tail(command: &mut Command, limit: Duration, most: u64) -> io::Result<(Ran, u64)> {
    let (kills, keeps) = (Kills::Program, Keeps::Last { most });
    let Ok(ran) = running::<Infallible>(command, limit, None, kills, Stdio::null(), keeps)?;
    Ok(ran)
}

pub fn run_alone(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    let kills = Kills::Group;
    let (command, stdin) = (alone(command), Stdio::null());
    let Ok((ran, _)) = running::<Infallible>(command, limit, None, kills, stdin, FIRST)?;
    Ok(ran)
}

pub fn run_while(
    command: &mut Command,
    limit: Duration,
    wanted: impl Fn() -> bool,
) -> io::Result<Option<Ran>> {
    run_while_reading(command, limit, wanted, Stdio::null())
}

pub fn run_while_reading(
    command: &mut Command,
    limit: Duration,
    wanted: impl Fn() -> bool,
    stdin: Stdio,
) -> io::Result<Option<Ran>> {
    let wanted = || if wanted() { Ok(()) } else { Err(()) };
    let ran = running::<()>(command, limit, Some(&wanted), Kills::Program, stdin, FIRST);
    ran.map(|ran| ran.ok().map(|(ran, _)| ran))
}

#[derive(Clone, Copy)]
enum Kills {
    Program,
    Group,
}

#[derive(Clone, Copy)]
enum Keeps {
    First { most: u64, over: io::ErrorKind },
    Last { most: u64 },
}

type Said = (Vec<u8>, u64);

const FIRST: Keeps = Keeps::First { most: SAID_MAX, over: io::ErrorKind::FileTooLarge };

fn running<N>(
    command: &mut Command,
    limit: Duration,
    wanted: Option<&dyn Fn() -> Result<(), N>>,
    kills: Kills,
    stdin: Stdio,
    keeps: Keeps,
) -> io::Result<Result<(Ran, u64), N>> {
    let deadline = Instant::now() + limit;
    let mut child =
        command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let heard = hear(&mut child, deadline, wanted, keeps);
    if !matches!(heard, Ok(Heard::Ended { .. })) {
        kill(&mut child, kills)?;
    }
    let status = child.wait()?;
    Ok(match heard? {
        Heard::Ended { stdout: (stdout, before), stderr } => {
            Ok((Ran::Ended { status, stdout, stderr }, before))
        },
        Heard::Stuck => Ok((Ran::Stuck, 0)),
        Heard::Unwanted(not) => Err(not),
    })
}

enum Heard<N> {
    Ended { stdout: Said, stderr: Vec<u8> },
    Stuck,
    Unwanted(N),
}

fn hear<N>(
    child: &mut Child,
    deadline: Instant,
    wanted: Option<&dyn Fn() -> Result<(), N>>,
    keeps: Keeps,
) -> io::Result<Heard<N>> {
    let stdout = child.stdout.take().expect("its stdout is piped");
    let stdout = read(stdout, keeps)?;
    let stderr = child.stderr.take().expect("its stderr is piped");
    let stderr = read(stderr, Keeps::First { most: SAID_MAX, over: io::ErrorKind::Other })?;
    let ended = ends(child)?;
    let left = || deadline.saturating_duration_since(Instant::now());
    loop {
        if Instant::now() >= deadline {
            return Ok(Heard::Stuck);
        }
        if let Some(Err(not)) = wanted.map(|wanted| wanted()) {
            return Ok(Heard::Unwanted(not));
        }
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
        return Ok(Heard::Stuck);
    };
    Ok(Heard::Ended { stdout: stdout?, stderr: stderr?.0 })
}

fn kill(child: &mut Child, kills: Kills) -> io::Result<()> {
    match kills {
        Kills::Program => child.kill(),
        Kills::Group => kill_group(child),
    }
}

#[expect(unsafe_code, reason = "std has no safe way to start a command in a session of its own")]
pub fn alone(command: &mut Command) -> &mut Command {
    unsafe {
        command.pre_exec(|| process::setsid().map(|_| ()).map_err(io::Error::from));
    }
    command
}

fn kill_group(child: &Child) -> io::Result<()> {
    let pid = Pid::from_child(child);
    match process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(Errno::PERM) if has_ended(pid) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn has_ended(pid: Pid) -> bool {
    let now = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    matches!(process::waitid(WaitId::Pid(pid), now), Ok(Some(status)) if an_end(&status))
}

fn an_end(status: &WaitIdStatus) -> bool {
    status.exited() || status.killed() || status.dumped()
}

fn ends(child: &Child) -> io::Result<Receiver<io::Result<()>>> {
    let pid = Pid::from_child(child);
    let ended = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    in_thread(move || {
        loop {
            match process::waitid(WaitId::Pid(pid), ended) {
                Ok(Some(status)) if an_end(&status) => return Ok(()),
                Err(Errno::INTR) => {},
                Err(error) => return Err(error.into()),
                Ok(_) => thread::sleep(LOOK),
            }
        }
    })
}

fn read(
    mut pipe: impl Read + Send + 'static,
    keeps: Keeps,
) -> io::Result<Receiver<io::Result<Said>>> {
    in_thread(move || {
        let (most, over) = match keeps {
            Keeps::First { most, over } => (most, over),
            Keeps::Last { most } => return last_lines(&mut pipe, most),
        };
        let mut all = Vec::new();
        let read = (&mut pipe).take(most).read_to_end(&mut all);
        let past = read.and_then(|_| io::copy(&mut pipe, &mut io::sink()));
        match past {
            Ok(0) => Ok((all, 0)),
            Ok(_) => {
                let more = format!("it said more than {most} bytes");
                Err(io::Error::new(over, more))
            },
            Err(error) => Err(error),
        }
    })
}

fn last_lines(pipe: &mut impl Read, most: u64) -> io::Result<Said> {
    let most = usize::try_from(most).unwrap_or(usize::MAX);
    let (mut kept, mut before) = (Vec::new(), 0);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => kept.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(error) => return Err(error),
        }
        if kept.len() > most.saturating_mul(2) {
            before += let_go(&mut kept, most);
        }
    }
    if kept.len() > most {
        before += let_go(&mut kept, most);
    }
    Ok((kept, before))
}

fn let_go(kept: &mut Vec<u8>, most: usize) -> u64 {
    let cut = kept.len().saturating_sub(most);
    let start = kept[cut..].iter().position(|&byte| byte == b'\n');
    let start = start.map_or(kept.len(), |at| cut + at + 1);
    kept.drain(..start).fold(0, |lines, byte| lines + u64::from(byte == b'\n'))
}

fn in_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> io::Result<Receiver<T>> {
    let (said, heard) = mpsc::sync_channel(1);
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        let _ = said.send(work());
    })?;
    Ok(heard)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File, TryLockError};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use rustix::process::{self, Pid, Signal};

    use super::{
        Ran, SAID_MAX, alone, run, run_alone, run_reading, run_tail, run_up_to, run_while,
        run_while_reading, saying,
    };

    #[test]
    fn a_program_hears_what_it_is_told_and_need_not_listen() {
        let words = "x".repeat(1024 * 1024);
        let limit = Duration::from_secs(10);
        let ran = run_reading(&mut Command::new("cat"), limit, saying(&words).unwrap()).unwrap();
        assert_eq!(ran.answer().unwrap(), words.as_bytes());
        let ran = run_reading(&mut Command::new("true"), limit, saying(&words).unwrap()).unwrap();
        assert_eq!(ran.answer(), Ok(Vec::new()));
    }

    #[test]
    fn a_program_that_fails_can_say_why_on_either_stream() {
        let ran = |script: &str| {
            let mut sh = Command::new("sh");
            run(sh.args(["-c", script]), Duration::from_secs(10)).unwrap()
        };
        let logged_out = "echo; echo 'Not logged in'; exit 1";
        assert_eq!(ran(logged_out).answer_why_on_either(), Err("Not logged in".to_owned()));
        assert_eq!(ran(logged_out).answer(), Err("exit status: 1".to_owned()));
        let both = "echo out; echo err >&2; exit 1";
        assert_eq!(ran(both).answer_why_on_either(), Err("out".to_owned()));
        assert_eq!(ran("echo err >&2; exit 1").answer_why_on_either(), Err("err".to_owned()));
        assert_eq!(ran("exit 3").answer_why_on_either(), Err("exit status: 3".to_owned()));
        assert_eq!(ran("echo fine").answer_why_on_either(), Ok(b"fine\n".to_vec()));
    }

    #[test]
    fn a_lock_on_what_a_program_reads_is_held_while_it_runs() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("lock");
        let lock = File::create(&path).unwrap();
        lock.lock().unwrap();
        let mut sh = Command::new("sh");
        sh.args(["-c", "exec 3<&0; sleep 0.5 >/dev/null 2>&1 &"]);
        let ran = run_while_reading(&mut sh, Duration::from_secs(5), || true, Stdio::from(lock));
        assert!(matches!(ran, Ok(Some(Ran::Ended { status, .. })) if status.success()));
        drop(sh);
        let other = File::open(&path).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        let free = Instant::now() + Duration::from_secs(5);
        while other.try_lock().is_err() {
            assert!(Instant::now() < free, "the lock outlived the program");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_program_that_says_too_much_has_failed() {
        let mut says = Command::new("head");
        says.args(["-c", &(SAID_MAX + 1).to_string(), "/dev/zero"]);
        let Err(error) = run(&mut says, Duration::from_secs(10)) else {
            panic!("it said more than the most, and was read");
        };
        assert!(error.to_string().contains("said more than"), "{error}");
    }

    #[test]
    fn a_program_may_say_as_much_as_it_is_asked_for() {
        let most = SAID_MAX + 1;
        let mut says = Command::new("head");
        says.args(["-c", &most.to_string(), "/dev/zero"]);
        let Ok(Ran::Ended { stdout, .. }) = run_up_to(&mut says, Duration::from_secs(10), most)
        else {
            panic!("it said no more than the most, and was not read");
        };
        assert_eq!(stdout.len() as u64, most);
        let Err(error) = run_up_to(&mut says, Duration::from_secs(10), most - 1) else {
            panic!("it said more than the most, and was read");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge, "{error}");
    }

    #[test]
    fn a_program_asked_for_its_last_lines_keeps_them() {
        let tail = |count: &str, most| {
            let mut says = Command::new("seq");
            says.arg(count);
            let Ok((Ran::Ended { stdout, .. }, before)) =
                run_tail(&mut says, Duration::from_secs(10), most)
            else {
                panic!("seq {count} was not heard");
            };
            (String::from_utf8(stdout).unwrap(), before)
        };
        let (kept, before) = tail("200000", 100);
        let last: Vec<String> = (199987..=200000).map(|line| format!("{line}\n")).collect();
        assert_eq!((kept, before), (last.concat(), 199986));
        assert_eq!(tail("3", 100), ("1\n2\n3\n".to_owned(), 0));
    }

    #[test]
    fn a_program_is_heard_from_as_it_ends() {
        let mut behind: Vec<Duration> = (0..61)
            .map(|_| {
                let asked = Instant::now();
                let said = run(&mut Command::new("true"), Duration::from_secs(10)).unwrap();
                let ran = asked.elapsed();
                assert_eq!(said.answer(), Ok(Vec::new()));
                let asked = Instant::now();
                assert!(Command::new("true").output().unwrap().status.success());
                ran.saturating_sub(asked.elapsed())
            })
            .collect();
        behind.sort();
        let least = behind[behind.len() / 5];
        assert!(least < Duration::from_millis(1), "{least:?} behind, of {behind:?}");
    }

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

    #[test]
    fn a_program_that_does_not_end_in_time_is_stuck() {
        let asked = Instant::now();
        let ran = run(Command::new("sleep").arg("10"), Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }

    #[test]
    fn a_program_no_longer_wanted_is_killed() {
        let asked = Instant::now();
        let wanted = || asked.elapsed() < Duration::from_millis(200);
        let ran = run_while(Command::new("sleep").arg("10"), Duration::from_secs(10), wanted);
        assert!(ran.unwrap().is_none());
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }

    #[test]
    fn a_program_that_stopped_is_stuck() {
        let asked = Instant::now();
        let mut stops = Command::new("sh");
        stops.args(["-c", "kill -STOP $$"]);
        let ran = run(&mut stops, Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
        assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    }

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
            let gone = Instant::now() + Duration::from_secs(5);
            while process::test_kill_process(pid).is_ok() {
                assert!(Instant::now() < gone, "what `{script}` started runs on");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn a_program_run_alone_held_from_outside_its_group_is_stuck_and_waited_for() {
        let dir = tempfile::tempdir().expect("a folder");
        let (ran_as, started) = (dir.path().join("sh"), dir.path().join("started"));
        let mut sh = Command::new("sh");
        sh.args(["-c", r#"echo $$ > "$0"; set -m; sleep 30 & echo $! > "$1""#]);
        let ran = run_alone(sh.arg(&ran_as).arg(&started), Duration::from_millis(500));
        let pid = |path| {
            let pid = fs::read_to_string(path).expect("sh says its pid and what it started");
            Pid::from_raw(pid.trim().parse().expect("a pid")).expect("a pid")
        };
        process::kill_process(pid(&started), Signal::KILL).expect("what sh started runs on");
        assert!(matches!(ran, Ok(Ran::Stuck)), "{:?}", ran.err());
        assert!(process::test_kill_process(pid(&ran_as)).is_err(), "sh was not waited for");
    }
}
