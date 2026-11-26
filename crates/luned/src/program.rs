use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write as _};
use std::mem;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus};

use crate::terminate::LUNED;

const LOOK: Duration = Duration::from_millis(5);

pub const SAID_MAX: u64 = 16 * 1024 * 1024;

const WATCHERS_MAX: usize = 16;

const WAITID_RETRIES_MAX: usize = 100_000;

const _: () = assert!(WATCHERS_MAX > 0);
const _: () = assert!(WAITID_RETRIES_MAX > 0);

type Hook = Box<dyn FnOnce() + Send>;

enum Waiters {
    Wanted { next: u64, hooks: Vec<(u64, Hook)> },
    GivenUp,
}

pub struct Cancel(Arc<Mutex<Waiters>>);

#[derive(Clone)]
pub struct Wanted(WantedInner);

#[derive(Clone)]
enum WantedInner {
    Always,
    Cancellable(Arc<Mutex<Waiters>>),
}

#[must_use]
pub struct Watching(Option<(Arc<Mutex<Waiters>>, u64)>);

#[must_use]
pub fn cancel() -> (Cancel, Wanted) {
    let waiters = Arc::new(Mutex::new(Waiters::Wanted { next: 0, hooks: Vec::new() }));
    (Cancel(Arc::clone(&waiters)), Wanted(WantedInner::Cancellable(waiters)))
}

impl Cancel {
    pub fn give_up(&self) {
        let hooks = {
            let mut waiters = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            match mem::replace(&mut *waiters, Waiters::GivenUp) {
                Waiters::Wanted { hooks, .. } => hooks,
                Waiters::GivenUp => return,
            }
        };
        assert!(hooks.len() <= WATCHERS_MAX, "waiters never exceed their bound");
        for (_, wake) in hooks {
            wake();
        }
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.give_up();
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wanted = matches!(
            *self.0.lock().unwrap_or_else(PoisonError::into_inner),
            Waiters::Wanted { .. }
        );
        f.debug_struct("Cancel").field("wanted", &wanted).finish()
    }
}

impl Wanted {
    #[must_use]
    pub fn always() -> Wanted {
        Wanted(WantedInner::Always)
    }

    #[must_use]
    pub fn still(&self) -> bool {
        match &self.0 {
            WantedInner::Always => true,
            WantedInner::Cancellable(waiters) => matches!(
                *waiters.lock().unwrap_or_else(PoisonError::into_inner),
                Waiters::Wanted { .. }
            ),
        }
    }

    pub fn on_give_up(&self, wake: impl FnOnce() + Send + 'static) -> Watching {
        let WantedInner::Cancellable(shared) = &self.0 else {
            return Watching(None);
        };
        let mut waiters = shared.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *waiters {
            Waiters::Wanted { next, hooks } => {
                assert!(
                    hooks.len() < WATCHERS_MAX,
                    "at most {WATCHERS_MAX} waiters listen for one cancellation"
                );
                let id = *next;
                *next = next.checked_add(1).expect("waiter ids fit a u64");
                hooks.push((id, Box::new(wake)));
                drop(waiters);
                Watching(Some((Arc::clone(shared), id)))
            },
            Waiters::GivenUp => {
                drop(waiters);
                wake();
                Watching(None)
            },
        }
    }

    #[must_use]
    pub fn wait_for(&self, span: Duration) -> bool {
        if matches!(self.0, WantedInner::Always) {
            thread::sleep(span);
            return true;
        }
        if !self.still() {
            return false;
        }
        let (wake, heard) = mpsc::sync_channel(1);
        let _watching = self.on_give_up(move || match wake.try_send(()) {
            Ok(()) | Err(TrySendError::Disconnected(())) => {},
            Err(TrySendError::Full(())) => {
                unreachable!("wait_for sends at most one cancellation wake");
            },
        });
        match heard.recv_timeout(span) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => false,
            Err(RecvTimeoutError::Timeout) => self.still(),
        }
    }

    pub async fn given_up(&self) {
        if matches!(self.0, WantedInner::Always) {
            std::future::pending::<()>().await;
            return;
        }
        let (wake, heard) = tokio::sync::oneshot::channel();
        let _watching = self.on_give_up(move || {
            let _ = wake.send(());
        });
        let _ = heard.await;
    }
}

impl fmt::Debug for Wanted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wanted = self.still();
        f.debug_struct("Wanted").field("wanted", &wanted).finish()
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some((waiters, id)) = self.0.take() {
            let mut waiters = waiters.lock().unwrap_or_else(PoisonError::into_inner);
            if let Waiters::Wanted { hooks, .. } = &mut *waiters {
                hooks.retain(|(kept, _)| *kept != id);
            }
        }
    }
}

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

pub fn luned() -> Result<PathBuf, String> {
    let this = env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("this program cannot tell where it is: {error}"))?;
    if this.file_name().is_some_and(|name| name == LUNED) {
        return Ok(this);
    }
    let beside = this.with_file_name(LUNED);
    if beside.is_file() {
        return Ok(beside);
    }
    Err(format!("no luned is beside {}", this.display()))
}

pub fn run(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    run_up_to(command, limit, SAID_MAX)
}

pub fn run_reading(command: &mut Command, limit: Duration, stdin: Stdio) -> io::Result<Ran> {
    let kills = Kills::Program;
    let (ran, _) = running_always(command, limit, kills, stdin, FIRST)?;
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
    let (ran, _) = running_always(command, limit, kills, Stdio::null(), keeps)?;
    Ok(ran)
}

pub fn run_tail(command: &mut Command, limit: Duration, most: u64) -> io::Result<(Ran, u64)> {
    let (kills, keeps) = (Kills::Program, Keeps::Last { most });
    running_always(command, limit, kills, Stdio::null(), keeps)
}

pub fn run_prefix(command: &mut Command, limit: Duration, most: u64) -> io::Result<(Ran, u64)> {
    assert!(most > 0, "prefix byte cap must be positive");
    let (kills, keeps) = (Kills::Program, Keeps::Prefix { most });
    running_always(command, limit, kills, Stdio::null(), keeps)
}

pub fn run_alone(command: &mut Command, limit: Duration) -> io::Result<Ran> {
    let kills = Kills::Group;
    let (command, stdin) = (alone(command), Stdio::null());
    let (ran, _) = running_always(command, limit, kills, stdin, FIRST)?;
    Ok(ran)
}

pub fn run_while(
    command: &mut Command,
    limit: Duration,
    wanted: &Wanted,
) -> io::Result<Option<Ran>> {
    run_while_reading(command, limit, wanted, Stdio::null())
}

pub fn run_while_reading(
    command: &mut Command,
    limit: Duration,
    wanted: &Wanted,
    stdin: Stdio,
) -> io::Result<Option<Ran>> {
    let ran = running(command, limit, Some(wanted), Kills::Program, stdin, FIRST)?;
    Ok(ran.map(|(ran, _)| ran))
}

fn running_always(
    command: &mut Command,
    limit: Duration,
    kills: Kills,
    stdin: Stdio,
    keeps: Keeps,
) -> io::Result<(Ran, u64)> {
    match running(command, limit, None, kills, stdin, keeps)? {
        Some(ran) => Ok(ran),
        None => unreachable!("a program wanted all along is never given up"),
    }
}

#[derive(Clone, Copy)]
enum Kills {
    Program,
    Group,
}

#[derive(Clone, Copy)]
enum Keeps {
    First { most: u64, over: io::ErrorKind },
    Prefix { most: u64 },
    Last { most: u64 },
}

type Said = (Vec<u8>, u64);

const FIRST: Keeps = Keeps::First { most: SAID_MAX, over: io::ErrorKind::FileTooLarge };

fn running(
    command: &mut Command,
    limit: Duration,
    wanted: Option<&Wanted>,
    kills: Kills,
    stdin: Stdio,
    keeps: Keeps,
) -> io::Result<Option<(Ran, u64)>> {
    if wanted.is_some_and(|wanted| !wanted.still()) {
        return Ok(None);
    }
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
            Some((Ran::Ended { status, stdout, stderr }, before))
        },
        Heard::Stuck => Some((Ran::Stuck, 0)),
        Heard::Unwanted => None,
    })
}

enum Heard {
    Ended { stdout: Said, stderr: Vec<u8> },
    Stuck,
    Unwanted,
}

enum Wait {
    Ended(io::Result<()>),
    Stdout(io::Result<Said>),
    Stderr(io::Result<Said>),
    Unwanted,
}

fn send_wait(told: &SyncSender<Wait>, wait: Wait) {
    match told.try_send(wait) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {},
        Err(TrySendError::Full(_)) => {
            unreachable!("hear channel holds one message per sender");
        },
    }
}

fn hear(
    child: &mut Child,
    deadline: Instant,
    wanted: Option<&Wanted>,
    keeps: Keeps,
) -> io::Result<Heard> {
    let (told, heard) = mpsc::sync_channel(4);
    let _watching = wanted.map(|wanted| {
        let told = told.clone();
        wanted.on_give_up(move || {
            send_wait(&told, Wait::Unwanted);
        })
    });
    let stdout = child.stdout.take().expect("its stdout is piped");
    read(stdout, keeps, told.clone(), Wait::Stdout)?;
    let stderr = child.stderr.take().expect("its stderr is piped");
    read(stderr, Keeps::First { most: SAID_MAX, over: io::ErrorKind::Other }, told.clone(), Wait::Stderr)?;
    ends(child, told)?;
    let (mut ended, mut out, mut err) = (false, None, None);
    for _ in 0..3 {
        let left = deadline.saturating_duration_since(Instant::now());
        match heard.recv_timeout(left) {
            Ok(Wait::Ended(end)) => {
                assert!(!ended, "a program ends once");
                end?;
                ended = true;
            },
            Ok(Wait::Stdout(said)) => {
                assert!(out.is_none(), "a program's stdout ends once");
                out = Some(said);
            },
            Ok(Wait::Stderr(said)) => {
                assert!(err.is_none(), "a program's stderr ends once");
                err = Some(said);
            },
            Ok(Wait::Unwanted) => return Ok(Heard::Unwanted),
            Err(RecvTimeoutError::Timeout) => return Ok(Heard::Stuck),
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the threads that read and wait for a program say how that went")
            },
        }
    }
    assert!(ended, "the program ended");
    let stdout = out.expect("stdout finished")?;
    let stderr = err.expect("stderr finished")?.0;
    Ok(Heard::Ended { stdout, stderr })
}

fn kill(child: &mut Child, kills: Kills) -> io::Result<()> {
    match kills {
        Kills::Program => child.kill(),
        Kills::Group => kill_group(child),
    }
}

#[expect(
    unsafe_code,
    reason = "std has no safe way to start a command in a session of its own. The closure runs \
              in the child, between its fork and its exec, where only what a signal handler may \
              do is safe: `setsid` is a system call alone, and its error holds no memory"
)]
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

fn ends(child: &Child, told: SyncSender<Wait>) -> io::Result<()> {
    let pid = Pid::from_child(child);
    let ended = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        let mut end = Err(io::Error::other("waitid did not observe process exit"));
        for _ in 0..WAITID_RETRIES_MAX {
            match process::waitid(WaitId::Pid(pid), ended) {
                Ok(Some(status)) if an_end(&status) => {
                    end = Ok(());
                    break;
                },
                Err(Errno::INTR) => {},
                Err(error) => {
                    end = Err(error.into());
                    break;
                },
                Ok(_) => thread::sleep(LOOK),
            }
        }
        send_wait(&told, Wait::Ended(end));
    })?;
    Ok(())
}

fn read(
    mut pipe: impl Read + Send + 'static,
    keeps: Keeps,
    told: SyncSender<Wait>,
    wrap: fn(io::Result<Said>) -> Wait,
) -> io::Result<()> {
    thread::Builder::new().name("program".to_owned()).spawn(move || {
        let said = match keeps {
            Keeps::First { most, over } => first_bytes(&mut pipe, most, Some(over)),
            Keeps::Prefix { most } => first_bytes(&mut pipe, most, None),
            Keeps::Last { most } => last_lines(&mut pipe, most),
        };
        send_wait(&told, wrap(said));
    })?;
    Ok(())
}

fn first_bytes(pipe: &mut impl Read, most: u64, over: Option<io::ErrorKind>) -> io::Result<Said> {
    let mut all = Vec::new();
    let read = (&mut *pipe).take(most).read_to_end(&mut all);
    let past = read.and_then(|_| io::copy(pipe, &mut io::sink()));
    match (past, over) {
        (Ok(0), _) => Ok((all, 0)),
        (Ok(extra), None) => Ok((all, extra)),
        (Ok(_), Some(over)) => {
            let more = format!("it said more than {most} bytes");
            Err(io::Error::new(over, more))
        },
        (Err(error), _) => Err(error),
    }
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
pub(crate) mod tests {
    use std::fs::{self, File, TryLockError};
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use lune_waits::{Began, Lasted, OUTLASTING, eventually};
    use rustix::process::{self, Pid, Signal};

    use super::{
        Ran, SAID_MAX, Wanted, alone, cancel, run, run_alone, run_reading, run_tail, run_up_to,
        run_while, run_while_reading, saying,
    };

    const _: () = assert!(OUTLASTING.subsec_nanos() == 0, "sleep is told whole seconds");

    pub(crate) fn outlasting_patience(ready: &Path) -> Command {
        let mut sleeps = Command::new("sh");
        sleeps.args([
            "-c",
            r#"echo > "$1"; exec sleep "$2""#,
            "sh",
            ready.to_str().unwrap(),
            &OUTLASTING.as_secs().to_string(),
        ]);
        sleeps
    }

    pub(crate) fn given_up<T: Send + 'static>(
        what: &str,
        run: impl FnOnce(&Path, &Wanted) -> T + Send + 'static,
    ) -> T {
        let scratch = tempfile::tempdir().unwrap();
        let ready = scratch.path().join("ready");
        let started = ready.clone();
        let (cancel, wanted) = cancel();
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || {
            let ran = run(&started, &wanted);
            ended.send(ran).unwrap();
        });
        eventually("the program is running before it is given up", || ready.exists().then_some(()));
        cancel.give_up();
        lune_waits::heard(what, &heard)
    }

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
        let ran = run_while_reading(&mut sh, Duration::from_secs(5), &Wanted::always(), Stdio::from(lock));
        assert!(matches!(ran, Ok(Some(Ran::Ended { status, .. })) if status.success()));
        drop(sh);
        let other = File::open(&path).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        eventually("the lock frees as the program ends", || other.try_lock().ok());
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
        let mut behind: Vec<Lasted> = (0..61)
            .map(|_| {
                let asked = Began::now();
                let said = run(&mut Command::new("true"), Duration::from_secs(10)).unwrap();
                let ran = asked.lasted();
                assert_eq!(said.answer(), Ok(Vec::new()));
                let asked = Began::now();
                assert!(Command::new("true").output().unwrap().status.success());
                ran.beyond(asked.lasted())
            })
            .collect();
        behind.sort();
        let least = behind[behind.len() / 5];
        assert!(least.is_under(Duration::from_millis(1)), "{least:?} behind, of {behind:?}");
    }

    #[test]
    fn a_program_a_signal_kills_is_heard_from_as_it_ends() {
        let mut dies = Command::new("sh");
        dies.args(["-c", "kill -KILL $$"]);
        let Ran::Ended { status, .. } = run(&mut dies, Duration::from_secs(10)).unwrap() else {
            panic!("it was not heard to end");
        };
        assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));
    }

    #[test]
    fn a_program_that_does_not_end_in_time_is_stuck() {
        let ran = run(Command::new("sleep").arg("10"), Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
    }

    #[test]
    fn a_program_no_longer_wanted_is_killed() {
        let ran = given_up("a program that ends only if killed ends", |ready, wanted| {
            run_while(&mut outlasting_patience(ready), OUTLASTING, wanted)
        });
        assert!(ran.unwrap().is_none());
    }

    #[test]
    fn a_program_that_stopped_is_stuck() {
        let mut stops = Command::new("sh");
        stops.args(["-c", "kill -STOP $$"]);
        let ran = run(&mut stops, Duration::from_millis(200)).unwrap();
        assert!(matches!(ran, Ran::Stuck));
    }

    #[test]
    fn a_program_run_alone_has_a_session_of_its_own() {
        let mut sleeps = Command::new("sleep");
        let mut child = alone(sleeps.arg("10")).spawn().expect("sleep runs");
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
            let ends = format!("what `{script}` started ends");
            eventually(&ends, || process::test_kill_process(pid).err());
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

    #[test]
    fn a_cancel_wakes_waiters_once_and_drops_unwatched_hooks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (control, wanted) = cancel();
        assert!(wanted.still());
        let fired = Arc::new(AtomicUsize::new(0));
        let dropped_fired = Arc::new(AtomicUsize::new(0));
        let keep_fired = Arc::clone(&fired);
        let _kept = wanted.on_give_up(move || {
            keep_fired.fetch_add(1, Ordering::SeqCst);
        });
        let drop_fired = Arc::clone(&dropped_fired);
        let removed = wanted.on_give_up(move || {
            drop_fired.fetch_add(1, Ordering::SeqCst);
        });
        drop(removed);

        let waiting = wanted.clone();
        let (done, heard) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let still = waiting.wait_for(OUTLASTING);
            done.send(still).unwrap();
        });
        lune_waits::never("wait_for stays parked while wanted", || heard.try_recv().ok());

        control.give_up();
        control.give_up();
        assert!(!wanted.still());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(dropped_fired.load(Ordering::SeqCst), 0);
        assert!(!lune_waits::heard("wait_for wakes on give_up", &heard));
        waiter.join().unwrap();

        let after_fired = Arc::new(AtomicUsize::new(0));
        let late = Arc::clone(&after_fired);
        let _late = wanted.on_give_up(move || {
            late.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(after_fired.load(Ordering::SeqCst), 1);
        assert!(!wanted.wait_for(OUTLASTING));

        let (drop_cancel, drop_wanted) = cancel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(drop_wanted.wait_for(Duration::from_millis(1)));
        drop(drop_cancel);
        assert!(!drop_wanted.still());
        runtime.block_on(lune_waits::awaited(
            "given_up resolves once its cancel drops",
            drop_wanted.given_up(),
        ));

        let always = Wanted::always();
        let many: Vec<_> = (0..=super::WATCHERS_MAX).map(|_| always.on_give_up(|| {})).collect();
        assert!(always.still());
        assert!(always.wait_for(Duration::from_millis(1)));
        drop(many);
    }

    #[test]
    fn a_program_asked_for_its_prefix_keeps_up_to_the_bound_and_counts_what_followed() {
        let mut sh = Command::new("sh");
        sh.args(["-c", "printf 'abcdefghij'"]);
        let (ran, extra) = super::run_prefix(&mut sh, Duration::from_secs(5), 4).unwrap();
        assert_eq!(ran.answer().unwrap(), b"abcd");
        assert_eq!(extra, 6);
    }
}
