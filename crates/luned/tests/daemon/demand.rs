use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use lune_waits::{eventually, Began, Span};
use luned::api::daemon_server::{Daemon, DaemonServer};
use luned::api::{
    self, AskRequest, AskResponse, FileIssueRequest, FileIssueResponse, IssueRepositoriesRequest,
    IssueRepositoriesResponse, InfoResponse, KeepChecksRequest, KeepChecksResponse,
    KeepImageRequest, KeepImageResponse, KeepPrintedRequest, KeepPrintedResponse,
    ListAgentsResponse, ListContentsRequest,
    ListContentsResponse, ListRepositoriesRequest, ListRepositoriesResponse, ListSkillsRequest,
    ListSkillsResponse,
    PasteRequest, PasteResponse, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse,
    RebaseRequest, RebaseResponse,
    TranscriptEndsRequest, TranscriptEndsResponse, Version, WaitingRequest, WaitingResponse,
    WatchRequest, AnswerRequest,
};
use luned::{folder, release, Claim, ALONE, SWEEP};
use prost::Message as _;
use rustix::process::{Pid, Signal};
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{pending, StreamExt as _};
use tonic::{Request, Response, Status, Streaming};

use crate::host::{
    answered, folder_in, info, install, script, side_by_side, start, start_as, Host, Running, Slot,
    Vars, Waits, LUNE, LUNED, PATIENT, SOCKET,
};
use crate::terminate::gone;

const OLDER: &str = "0.0.1";

const NEWER: &str = "99.0.0";

const NONE_RUNS: i32 = 5;

const STAND_IN: &str = "LUNE_TEST_STAND_IN_FOLDER";

const STAND_IN_VERSION: &str = "LUNE_TEST_STAND_IN_VERSION";

const STAND_IN_PID: &str = "LUNE_TEST_STAND_IN_PID";

const STAND_IN_BRIDGED: &str = "LUNE_TEST_STAND_IN_BRIDGED_VERSION";

const BRIDGED_FD: &str = "/dev/fd/3";

fn watch(host: &Host, lune: &Path) -> api::Host {
    written(host.vars().command(&lune.display().to_string()).arg("watch"))
}

fn refused(host: &Host, lune: &Path) -> String {
    let output = answered(host.vars().command(&lune.display().to_string()).arg("watch"));
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "lune watch was not refused: {said}");
    said
}

fn written(command: &mut Command) -> api::Host {
    let output = answered(command);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "lune watch failed: {said}");
    api::Host::decode(output.stdout.as_slice()).unwrap()
}

fn pid(host: &api::Host) -> u32 {
    host.version.unwrap().pid
}

pub fn answering(host: &Host) -> Option<u32> {
    told(host).map(|info| info.pid)
}

fn told(host: &Host) -> Option<InfoResponse> {
    let folder = folder_in(host.runtime.path())?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.ok()?;
        Some(daemon.info(()).await.ok()?.into_inner())
    })
}

fn held(host: &Host) -> bool {
    folder_in(host.runtime.path()).is_some_and(|folder| folder::held(&folder).unwrap())
}

pub fn stop(pid: u32) {
    let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::kill_process(pid, Signal::TERM).unwrap();
}

fn log(host: &Host) -> String {
    fs::read_to_string(host.folder().join(folder::LOG)).unwrap()
}

pub fn moved() -> tempfile::TempDir {
    let elsewhere = tempfile::tempdir().unwrap();
    install(Path::new(LUNE.as_str()), &elsewhere.path().join("lune"));
    elsewhere
}

pub struct Detached<'a> {
    pub host: &'a Host,
    pub pid: u32,
}

impl Drop for Detached<'_> {
    fn drop(&mut self) {
        if answering(self.host) == Some(self.pid) {
            stop(self.pid);
        }
    }
}

#[test]
fn lune_watch_starts_a_detached_daemon_where_none_runs() {
    let host = Host::new();
    let mut handed = host.vars().command("sh");
    handed.args(["-c", "exec \"$0\" watch 7>&1", LUNE.as_str()]);
    let first = written(&mut handed);
    let daemon = Detached { host: &host, pid: pid(&first) };

    let leader = Pid::from_raw(i32::try_from(daemon.pid).unwrap());
    assert_eq!(rustix::process::getsid(leader).unwrap(), leader.unwrap());
    let log = log(&host);
    assert!(log.contains("luned: listening on "), "{log}");
    assert_eq!(pid(&watch(&host, Path::new(LUNE.as_str()))), daemon.pid);
}

#[test]
fn luned_says_in_lunes_log_when_and_which_daemon() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    let log = fs::read_to_string(host.home.path().join(".local/state/lune/lune.log")).unwrap();
    let listening = format!("luned[{}] listening on ", daemon.pid());
    let line = log.lines().find(|line| line.contains(&listening)).expect(&log);
    let (when, said) = line.split_once(' ').unwrap();
    assert!(said.starts_with(&listening), "{line}");
    let date = |at: usize| when.as_bytes().get(at).copied();
    assert_eq!((when.len(), date(10), date(23)), (24, Some(b'T'), Some(b'Z')), "{when}");
}

#[test]
fn luned_with_no_log_says_first_why_it_does_not_start() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("r".repeat(110));
    fs::create_dir(&runtime).unwrap();
    let (ended, said) = start(Vars::runtime(&runtime)).err().unwrap();
    assert!(!ended.success());
    let socket = runtime.join(SOCKET);
    let first = said.lines().next().unwrap_or_default();
    assert!(first.starts_with(&format!("luned: {}: ", socket.display())), "{said}");
}

#[test]
fn the_cli_says_that_no_daemon_runs_and_starts_none() {
    let host = Host::new();
    let none_runs = |host: &Host| {
        for command in ["info", "agents"] {
            let output = answered(host.vars().command(&LUNE).arg(command));
            let said = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(NONE_RUNS), "{command}: {said}");
            let none = format!("lune: no luned runs in {}", host.folder().display());
            assert!(said.contains(&none), "{command}: {said}");
        }
        assert_eq!(answering(host), None);
        assert!(!held(host));
    };
    none_runs(&host);

    drop(start(host.vars()).unwrap());
    assert!(host.runtime.path().join(SOCKET).exists(), "a crash leaves the socket behind");
    none_runs(&host);
}

#[test]
fn lune_starts_a_fresh_daemon_in_the_place_of_one_that_aborted() {
    let host = Host::new();
    let mut undumped = host.vars().command("sh");
    undumped.args(["-c", "ulimit -c 0 && exec \"$0\"", LUNED]);
    let mut aborted = start_as(undumped).unwrap();
    let aborting = Pid::from_raw(i32::try_from(aborted.pid()).unwrap()).unwrap();
    rustix::process::kill_process(aborting, Signal::ABORT).unwrap();
    let ended = aborted.ended("luned aborts");
    assert_eq!(ended.signal(), Some(Signal::ABORT.as_raw()), "{ended}");
    assert!(host.runtime.path().join(SOCKET).exists(), "an abort leaves the socket behind");

    let fresh = pid(&watch(&host, Path::new(LUNE.as_str())));
    let _fresh = Detached { host: &host, pid: fresh };
    assert_ne!(fresh, aborted.pid());
    assert_eq!(answering(&host), Some(fresh));
}

#[test]
fn lunes_that_start_a_daemon_at_once_reach_the_same_one() {
    const LUNES: usize = 12;
    let host = Host::new();
    let elsewhere = moved();
    let spawned = elsewhere.path().join("spawned");
    let stand_in = format!(
        "echo >> '{spawned}'\n\
         waited=0\n\
         while [ $(wc -l < '{spawned}') -lt {LUNES} ] && [ $waited -lt 300 ]; do\n\
         \x20   sleep 0.01\n\
         \x20   waited=$((waited + 1))\n\
         done\n\
         exec '{LUNED}' \"$@\"\n",
        spawned = spawned.display()
    );
    script(&elsewhere.path().join("luned"), &stand_in);
    let lune = elsewhere.path().join("lune");
    let pids: Vec<u32> = thread::scope(|scope| {
        let asking: Vec<_> =
            (0..LUNES).map(|_| scope.spawn(|| pid(&watch(&host, &lune)))).collect();
        asking.into_iter().map(|asked| asked.join().unwrap()).collect()
    });
    let _daemon = Detached { host: &host, pid: pids[0] };
    assert!(pids.iter().all(|pid| *pid == pids[0]), "{pids:?}");
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), LUNES);
    let lost = || log(&host).matches("another luned already runs").count();
    eventually("the other luneds lose", || (lost() == LUNES - 1).then_some(()));
    let log = log(&host);
    assert_eq!(log.matches("luned: listening on ").count(), 1, "{log}");
}

#[test]
fn a_lune_whose_daemon_finds_the_folder_held_waits_for_it() {
    let host = Host::new();
    let elsewhere = moved();
    let (hold, spawned) = (elsewhere.path().join("hold"), elsewhere.path().join("spawned"));
    let stand_in = format!(
        "echo >> '{}'\nwhile [ -e '{}' ]; do sleep 0.01; done\nexec '{LUNED}' \"$@\"\n",
        spawned.display(),
        hold.display()
    );
    script(&elsewhere.path().join("luned"), &stand_in);
    fs::write(&hold, "").unwrap();
    let lune = elsewhere.path().join("lune");

    let answer = thread::scope(|scope| {
        let asking = scope.spawn(|| watch(&host, &lune));
        eventually("lune starts a luned", || spawned.exists().then_some(()));
        let claim = Claim::take(&host.folder()).unwrap();
        fs::remove_file(&hold).unwrap();
        eventually("the luned finds the folder held", || {
            log(&host).contains("another luned already runs").then_some(())
        });
        drop(claim);
        asking.join().unwrap()
    });
    let _daemon = Detached { host: &host, pid: pid(&answer) };
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), 2);
}

fn stopped_as_it_starts(elsewhere: &tempfile::TempDir) -> PathBuf {
    let spawned = elsewhere.path().join("spawned");
    let stand_in = format!(
        "echo $$ >> '{}'\necho 'luned: listening on it' >&2\nkill -TERM $$\n",
        spawned.display()
    );
    script(&elsewhere.path().join("luned"), &stand_in);
    spawned
}

#[test]
fn a_lune_whose_luned_is_stopped_as_it_starts_waits_for_the_next() {
    let host = Host::new();
    let elsewhere = moved();
    let spawned = stopped_as_it_starts(&elsewhere);
    let lune = elsewhere.path().join("lune");

    thread::scope(|scope| {
        let asking = scope.spawn(|| watch(&host, &lune));
        let stopped = eventually("lune starts a luned", || {
            fs::read_to_string(&spawned).ok()?.trim().parse::<u32>().ok()
        });
        eventually("lune sees its luned stopped", || gone(stopped).then_some(()));
        let next = start(host.vars()).unwrap();
        assert_eq!(pid(&asking.join().unwrap()), next.pid());
    });
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), 1);
}

#[test]
fn a_lune_whose_luned_is_stopped_as_it_starts_says_it_was_stopped() {
    let host = Host::new();
    let elsewhere = moved();
    let spawned = stopped_as_it_starts(&elsewhere);
    let lune = elsewhere.path().join("lune");

    let output = answered(host.vars().command(&lune.display().to_string()).arg("watch"));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("the luned lune started was stopped"), "{said}");
    assert!(!said.contains("did not start"), "{said}");
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), 1);
}

#[test]
fn lune_starts_the_luned_beside_it_or_else_the_one_on_the_path() {
    let host = Host::new();
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let stand_in = |name: &str| {
        format!("echo {name} >> '{}'\nexec '{LUNED}' \"$@\"\n", started.display())
    };
    script(&elsewhere.path().join("luned"), &stand_in("beside"));
    script(&host.bin().join("luned"), &stand_in("path"));
    let lune = elsewhere.path().join("lune");

    stop(pid(&watch(&host, &lune)));
    eventually("the daemon beside lune ends", || (!held(&host)).then_some(()));
    fs::remove_file(elsewhere.path().join("luned")).unwrap();
    let _daemon = Detached { host: &host, pid: pid(&watch(&host, &lune)) };
    assert_eq!(fs::read_to_string(&started).unwrap(), "beside\npath\n");
}

#[test]
fn lune_watch_starts_a_daemon_of_its_version_in_the_place_of_an_older_one() {
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None);
    let said = info(host.vars());
    assert!(said.starts_with(&format!("luned {OLDER} (pid {})", older.pid())), "{said}");

    let elsewhere = moved();
    let other = "if [ \"$1\" = --version ]; then echo 'luned 0.0.0'; exit; fi\nexit 1\n";
    let beside = elsewhere.path().join("luned");
    script(&beside, other);
    let said = refused(&host, &elsewhere.path().join("lune"));
    let real = beside.canonicalize().unwrap();
    let kept = format!("but {} is 0.0.0, no newer, so lune leaves it be", real.display());
    assert!(said.contains(&format!("luned {OLDER} (pid {})", older.pid())), "{said}");
    assert!(said.contains(&kept), "{said}");
    assert_eq!(older.exit_status(), None);

    let current = watch(&host, Path::new(LUNE.as_str()));
    let _daemon = Detached { host: &host, pid: pid(&current) };
    let ended = older.ended("lune stops the older daemon");
    assert_eq!(ended.signal(), Some(Signal::TERM.as_raw()));
    assert_ne!(pid(&current), older.pid());
    let said = info(host.vars());
    let current = release::spelled(release::VERSION, release::BUILT);
    assert!(said.starts_with(&format!("luned {current} ")), "{said}");
}

#[test]
fn lune_watch_goes_by_the_stamp_of_a_daemon_of_its_release() {
    let stamped = |built| release::spelled(release::VERSION, Some(built));

    let later = Host::new();
    let mut after = stand_in(&later, &stamped(u64::MAX), None);
    let said = refused(&later, Path::new(LUNE.as_str()));
    assert!(said.contains(&format!("is newer than this lune, {}", *release::SAID)), "{said}");
    assert_eq!(after.exit_status(), None);

    let host = Host::new();
    let mut before = stand_in(&host, &stamped(0), None);
    if release::BUILT.is_some() {
        let reached = pid(&watch(&host, Path::new(LUNE.as_str())));
        let _daemon = Detached { host: &host, pid: reached };
        before.ended("lune stops the stamped daemon");
        assert_ne!(reached, before.pid());
    } else {
        let said = refused(&host, Path::new(LUNE.as_str()));
        assert!(said.contains(&format!("luned {} (pid ", stamped(0))), "{said}");
        assert_eq!(before.exit_status(), None);
    }
}

#[test]
fn lune_and_luned_say_their_build_alike() {
    for (program, name) in [(LUNE.as_str(), "lune"), (LUNED, "luned")] {
        let said = Command::new(program).arg("--version").output().unwrap();
        assert!(said.status.success(), "{name} --version failed");
        assert_eq!(String::from_utf8_lossy(&said.stdout), format!("{name} {}\n", *release::SAID));
    }
}

#[test]
fn lunes_that_find_an_older_daemon_at_once_reach_the_one_in_its_place() {
    const LUNES: usize = 8;
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None);
    let pids: Vec<u32> = thread::scope(|scope| {
        let asking: Vec<_> = (0..LUNES)
            .map(|_| scope.spawn(|| pid(&watch(&host, Path::new(LUNE.as_str())))))
            .collect();
        asking.into_iter().map(|asked| asked.join().unwrap()).collect()
    });
    let _daemon = Detached { host: &host, pid: pids[0] };
    older.ended("lune stops the older daemon");
    assert!(pids.iter().all(|pid| *pid == pids[0] && *pid != older.pid()), "{pids:?}");
}

#[test]
fn lune_watch_refuses_a_daemon_newer_than_it_and_leaves_it_be() {
    let host = Host::new();
    let mut newer = stand_in(&host, NEWER, None);
    let said = refused(&host, Path::new(LUNE.as_str()));
    let (pid, own) = (newer.pid(), release::SAID.as_str());
    let both = format!("luned {NEWER} (pid {pid}) is newer than this lune, {own}");
    assert!(said.contains(&both), "{said}");
    assert!(said.contains(&format!("`kill {pid}`")), "{said}");
    assert_eq!(newer.exit_status(), None);
}

#[test]
fn the_cli_asks_no_daemon_of_another_build_and_info_says_it_is_one() {
    for version in [OLDER, NEWER] {
        let host = Host::new();
        let mut other = stand_in(&host, version, None);
        let output = answered(host.vars().command(&LUNE).arg("agents"));
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{said}");
        let theirs = format!("luned {version} (pid {})", other.pid());
        assert!(said.contains(&theirs), "{said}");
        assert!(said.contains(release::SAID.as_str()), "{said}");

        let output = answered(host.vars().command(&LUNE).arg("info"));
        let out = String::from_utf8_lossy(&output.stdout);
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{said}");
        assert!(out.starts_with(&theirs), "{out}");
        assert!(said.contains(&theirs) && said.contains(release::SAID.as_str()), "{said}");
        assert_eq!(other.exit_status(), None);
    }
}

#[test]
fn lune_watch_stops_no_process_but_the_one_the_kernel_says_listens() {
    let host = Host::new();
    let mut decoy = Running::spawn(Command::new("sleep").arg("60"));
    let mut older = stand_in(&host, OLDER, Some(decoy.pid()));
    let said = refused(&host, Path::new(LUNE.as_str()));
    let listens = "but the kernel says another process listens on its socket, so lune stops none";
    assert!(said.contains(listens), "{said}");
    assert_eq!(decoy.exit_status(), None);
    assert_eq!(older.exit_status(), None);
}

#[test]
fn the_daemon_says_the_folder_it_reads_claude_code_s_sessions_in() {
    let host = Host::new();
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    fs::create_dir_all(claude.join("sessions")).unwrap();
    let _daemon = start(Vars { home: Some(home.path()), ..host.vars() }).unwrap();
    let reads = told(&host).and_then(|info| info.claude_folder);
    assert_eq!(reads.as_deref(), Some(claude.as_os_str().as_bytes()));
}

fn stand_in(host: &Host, version: &str, says: Option<u32>) -> Running {
    let mut command = stand_in_command(host, version);
    if let Some(pid) = says {
        command.env(STAND_IN_PID, pid.to_string());
    }
    let stand_in = Running::spawn(&mut command);
    let said = says.unwrap_or(stand_in.pid());
    eventually("the stand-in answers", || (answering(host)? == said).then_some(()));
    stand_in
}

fn stand_in_command(host: &Host, version: &str) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["demand::stand_in_luned", "--exact", "--ignored"])
        .env(STAND_IN, host.folder())
        .env(STAND_IN_VERSION, version)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[test]
#[ignore = "the stand-in for a daemon of another version, which a test runs as a process"]
fn stand_in_luned() {
    let Some(folder) = env::var_os(STAND_IN) else {
        return;
    };
    let (version, built) = build_of(env::var(STAND_IN_VERSION).unwrap());
    let pid = env::var(STAND_IN_PID).map_or(std::process::id(), |pid| pid.parse().unwrap());
    let claim = Claim::take(Path::new(&folder)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::bind(claim.socket()).unwrap();
        tonic::transport::Server::builder()
            .add_service(DaemonServer::new(StandIn { version, built, pid }))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });
}

pub fn build_of(said: String) -> (String, Option<u64>) {
    match said.split_once('+') {
        Some((version, built)) => (version.to_owned(), Some(built.parse().unwrap())),
        None => (said, None),
    }
}

pub fn bridged_stand_in(version: &str) -> String {
    let program = env::current_exe().unwrap();
    let test = "demand::stand_in_bridged_luned --exact --ignored";
    let to = "3>&1 1>/dev/null 2>/dev/null";
    format!("exec env {STAND_IN_BRIDGED}='{version}' '{}' {test} {to}\n", program.display())
}

#[test]
#[ignore = "the stand-in for a daemon of another version that lune bridge hands on, which a test \
            runs as a process"]
fn stand_in_bridged_luned() {
    let Ok(said) = env::var(STAND_IN_BRIDGED) else {
        return;
    };
    let (version, built) = build_of(said);
    let mut out = OpenOptions::new().write(true).open(BRIDGED_FD).unwrap();
    out.write_all(luned::BRIDGED).unwrap();
    let (served, relayed) = UnixStream::pair().unwrap();
    let mut up = relayed.try_clone().unwrap();
    let mut down = relayed;
    thread::spawn(move || {
        relay(&mut io::stdin(), &mut up);
        std::process::exit(0);
    });
    thread::spawn(move || relay(&mut down, &mut out));
    served.set_nonblocking(true).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let served = tokio::net::UnixStream::from_std(served).unwrap();
        let pid = std::process::id();
        let stand_in = StandIn { version, built, pid };
        tonic::transport::Server::builder()
            .add_service(DaemonServer::new(stand_in))
            .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(served)).chain(pending()))
            .await
            .unwrap();
    });
}

fn relay(from: &mut impl Read, to: &mut impl Write) {
    let mut buffer = vec![0; 64 * 1024];
    while let Ok(read) = from.read(&mut buffer) {
        if read == 0 || to.write_all(&buffer[..read]).is_err() {
            return;
        }
    }
}

struct StandIn {
    version: String,
    built: Option<u64>,
    pid: u32,
}

#[tonic::async_trait]
impl Daemon for StandIn {
    async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
        let (version, built, pid) = (self.version.clone(), self.built, self.pid);
        let info = InfoResponse { version, built, pid, ..InfoResponse::default() };
        Ok(Response::new(info))
    }

    async fn list_agents(&self, _: Request<()>) -> Result<Response<ListAgentsResponse>, Status> {
        Ok(Response::new(ListAgentsResponse::default()))
    }

    async fn watch(&self, _: Request<WatchRequest>) -> Result<Response<api::Host>, Status> {
        let version = Version { pid: self.pid, seq: 0 };
        Ok(Response::new(api::Host { version: Some(version), ..api::Host::default() }))
    }

    type KeepChecksStream = tokio_stream::Empty<Result<KeepChecksResponse, Status>>;

    async fn keep_checks(
        &self,
        _: Request<KeepChecksRequest>,
    ) -> Result<Response<Self::KeepChecksStream>, Status> {
        Err(Status::unimplemented("a stand-in asks gh nothing"))
    }

    type FileIssueStream = tokio_stream::Empty<Result<FileIssueResponse, Status>>;

    async fn file_issue(
        &self,
        _: Request<FileIssueRequest>,
    ) -> Result<Response<Self::FileIssueStream>, Status> {
        Err(Status::unimplemented("a stand-in asks gh nothing"))
    }

    async fn issue_repositories(
        &self,
        _: Request<IssueRepositoriesRequest>,
    ) -> Result<Response<IssueRepositoriesResponse>, Status> {
        Err(Status::unimplemented("a stand-in reads no remotes"))
    }

    async fn change_projects(
        &self,
        _: Request<api::ChangeProjectsRequest>,
    ) -> Result<Response<api::ChangeProjectsResponse>, Status> {
        Err(Status::unimplemented("a stand-in keeps no projects"))
    }

    type MakeWorktreeStream = tokio_stream::Empty<Result<api::MakeWorktreeResponse, Status>>;

    async fn make_worktree(
        &self,
        _: Request<api::MakeWorktreeRequest>,
    ) -> Result<Response<Self::MakeWorktreeStream>, Status> {
        Err(Status::unimplemented("a stand-in makes no worktree"))
    }

    type ListWorkStream = tokio_stream::Empty<Result<api::ListWorkResponse, Status>>;

    async fn list_work(
        &self,
        _: Request<api::ListWorkRequest>,
    ) -> Result<Response<Self::ListWorkStream>, Status> {
        Err(Status::unimplemented("a stand-in lists no work"))
    }

    type ReadDiffStream = tokio_stream::Empty<Result<api::ReadDiffResponse, Status>>;

    async fn read_diff(
        &self,
        _: Request<api::ReadDiffRequest>,
    ) -> Result<Response<Self::ReadDiffStream>, Status> {
        Err(Status::unimplemented("a stand-in reads no diff"))
    }

    async fn list_skills(
        &self,
        _: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        Err(Status::unimplemented("a stand-in finds no skills"))
    }

    async fn list_repositories(
        &self,
        _: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        Err(Status::unimplemented("a stand-in knows no repository"))
    }

    async fn keep_printed(
        &self,
        _: Request<KeepPrintedRequest>,
    ) -> Result<Response<KeepPrintedResponse>, Status> {
        Err(Status::unimplemented("a stand-in keeps nothing printed"))
    }

    async fn list_contents(
        &self,
        _: Request<ListContentsRequest>,
    ) -> Result<Response<ListContentsResponse>, Status> {
        Err(Status::unimplemented("a stand-in lists nothing"))
    }

    async fn transcript_ends(
        &self,
        _: Request<TranscriptEndsRequest>,
    ) -> Result<Response<TranscriptEndsResponse>, Status> {
        Err(Status::unimplemented("a stand-in reads no transcripts"))
    }

    async fn waiting(
        &self,
        _: Request<WaitingRequest>,
    ) -> Result<Response<WaitingResponse>, Status> {
        Err(Status::unimplemented("a stand-in shows no screen"))
    }

    async fn answer(&self, _: Request<AnswerRequest>) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("a stand-in answers no agent"))
    }

    type AskStream = tokio_stream::Empty<Result<AskResponse, Status>>;

    async fn ask(&self, _: Request<AskRequest>) -> Result<Response<Self::AskStream>, Status> {
        Err(Status::unimplemented("a stand-in runs no agent"))
    }

    type PasteStream = tokio_stream::Empty<Result<PasteResponse, Status>>;

    async fn paste(&self, _: Request<PasteRequest>) -> Result<Response<Self::PasteStream>, Status> {
        Err(Status::unimplemented("a stand-in makes no worktree"))
    }

    type PrunesStream = tokio_stream::Empty<Result<PrunesResponse, Status>>;

    async fn prunes(&self, _: Request<PrunesRequest>) -> Result<Response<Self::PrunesStream>, Status> {
        Err(Status::unimplemented("a stand-in deletes no worktree"))
    }

    type PruneStream = tokio_stream::Empty<Result<PruneResponse, Status>>;

    async fn prune(&self, _: Request<PruneRequest>) -> Result<Response<Self::PruneStream>, Status> {
        Err(Status::unimplemented("a stand-in deletes no worktree"))
    }

    type RebaseStream = tokio_stream::Empty<Result<RebaseResponse, Status>>;

    async fn rebase(
        &self,
        _: Request<RebaseRequest>,
    ) -> Result<Response<Self::RebaseStream>, Status> {
        Err(Status::unimplemented("a stand-in rebases no branch"))
    }

    type KeepImageStream = tokio_stream::Empty<Result<KeepImageResponse, Status>>;

    async fn keep_image(
        &self,
        _: Request<Streaming<KeepImageRequest>>,
    ) -> Result<Response<Self::KeepImageStream>, Status> {
        Err(Status::unimplemented("a stand-in keeps no image"))
    }

    async fn say(&self, _: Request<api::SayRequest>) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("a stand-in reads nothing aloud"))
    }

    type ShowStream = tokio_stream::Empty<Result<api::ShowResponse, Status>>;

    async fn show(
        &self,
        _: Request<Streaming<api::ShowRequest>>,
    ) -> Result<Response<Self::ShowStream>, Status> {
        Err(Status::unimplemented("a stand-in shows no layout"))
    }

    type FramesStream = tokio_stream::Empty<Result<luned::mux::own::api::ToViewer, Status>>;

    async fn frames(
        &self,
        _: Request<Streaming<api::FramesRequest>>,
    ) -> Result<Response<Self::FramesStream>, Status> {
        Err(Status::unimplemented("a stand-in shows no terminal"))
    }

    async fn list_resume(
        &self,
        _: Request<api::ListResumeRequest>,
    ) -> Result<Response<api::ListResumeResponse>, Status> {
        Err(Status::unimplemented("a stand-in lists no resume records"))
    }

    async fn reload_resume(
        &self,
        _: Request<api::ReloadResumeRequest>,
    ) -> Result<Response<api::ReloadResumeResponse>, Status> {
        Err(Status::unimplemented("a stand-in reloads no resume rules"))
    }
}

#[test]
fn the_waits_of_half_a_minute_hold_side_by_side() {
    side_by_side(const {
        Waits::of([
            ("the daemon ends alone", the_daemon_ends_once_nothing_has_kept_it_for_a_while),
            ("a terminal keeps the daemon", crate::own::the_daemon_runs_on_while_a_terminal_does),
            ("a client keeps the daemon", the_daemon_runs_on_while_a_client_watches_it),
            (
                "a client keeps the machines it watches",
                crate::remote::a_daemon_a_client_keeps_keeps_the_machines_it_watches_running,
            ),
            (
                "two daemons that watch each other end",
                crate::remote::two_daemons_that_watch_each_other_and_nothing_else_end,
            ),
        ])
    });
}

fn the_daemon_ends_once_nothing_has_kept_it_for_a_while(_: Slot) {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    host.host();
    let _silent = UnixStream::connect(host.runtime.path().join(SOCKET)).unwrap();
    let alone = Began::now();
    let early = const { Span::of(ALONE.saturating_sub(SWEEP)) };
    daemon.runs_through(early, "the daemon ends before it is alone long enough");
    let ended = daemon.ended("the daemon ends once alone");
    let lasted = alone.lasted();

    assert!(ended.success(), "{ended}");
    lasted.at_least(ALONE, "the daemon ends only once alone that long");
    assert!(!host.runtime.path().join(SOCKET).exists());
    let none = format!("no terminal for {}s", ALONE.as_secs());
    daemon.says("the daemon says why it ends", |line| line.contains(&none));
}

fn the_daemon_runs_on_while_a_client_watches_it(_: Slot) {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut watching = luned::connect(&folder, PATIENT).await.unwrap();
        let mut after = None;
        let watched = const { Span::of(ALONE.saturating_add(SWEEP.saturating_mul(3))) };
        lune_waits::repeated_through(watched, "the daemon answers a watch", async || {
            let asked = watching.watch(WatchRequest { after, kept: None, shown: false }).await;
            after = asked.expect("the daemon ended").into_inner().version;
        })
        .await;
    });
    assert_eq!(daemon.exit_status(), None);
}
