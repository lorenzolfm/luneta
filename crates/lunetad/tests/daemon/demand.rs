use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::daemon_server::{Daemon, DaemonServer};
use lunetad::api::{
    self, AskRequest, AskResponse, FileIssueRequest, FileIssueResponse, IssueRepositoriesRequest,
    IssueRepositoriesResponse, InfoResponse, KeepChecksRequest, KeepChecksResponse,
    KeepImageRequest, KeepImageResponse, ListAgentsResponse, ListContentsRequest,
    ListContentsResponse, ListDirsRequest, ListDirsResponse, ListSkillsRequest, ListSkillsResponse,
    PasteRequest, PasteResponse, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse,
    TranscriptEndsRequest, TranscriptEndsResponse, Version, WatchRequest,
};
use lunetad::{folder, release, Claim};
use prost::Message as _;
use rustix::process::{Pid, Signal};
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{pending, StreamExt as _};
use tonic::{Request, Response, Status, Streaming};

use crate::host::{
    answered, eventually, folder_in, info, install, script, start, Host, Running, Vars, LUNETA,
    LUNETAD, SOCKET,
};

const OLDER: &str = "0.0.1";

const NEWER: &str = "99.0.0";

const NONE_RUNS: i32 = 5;

const STAND_IN: &str = "LUNETA_TEST_STAND_IN_FOLDER";

const STAND_IN_VERSION: &str = "LUNETA_TEST_STAND_IN_VERSION";

const STAND_IN_PID: &str = "LUNETA_TEST_STAND_IN_PID";

const STAND_IN_BRIDGED: &str = "LUNETA_TEST_STAND_IN_BRIDGED_VERSION";

const BRIDGED_FD: &str = "/dev/fd/3";

fn watch(host: &Host, luneta: &Path) -> api::Host {
    written(host.vars().command(&luneta.display().to_string()).arg("watch"))
}

fn refused(host: &Host, luneta: &Path) -> String {
    let output = answered(host.vars().command(&luneta.display().to_string()).arg("watch"));
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "luneta watch was not refused: {said}");
    said
}

fn written(command: &mut Command) -> api::Host {
    let output = answered(command);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "luneta watch failed: {said}");
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
        let mut daemon = lunetad::connect(&folder).await.ok()?;
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
    install(Path::new(LUNETA), &elsewhere.path().join("luneta"));
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
fn luneta_watch_starts_a_detached_daemon_where_none_runs() {
    let host = Host::new();
    let mut handed = host.vars().command("sh");
    handed.args(["-c", "exec \"$0\" watch 7>&1", LUNETA]);
    let first = written(&mut handed);
    let daemon = Detached { host: &host, pid: pid(&first) };

    let leader = Pid::from_raw(i32::try_from(daemon.pid).unwrap());
    assert_eq!(rustix::process::getsid(leader).unwrap(), leader.unwrap());
    let log = log(&host);
    assert!(log.contains("lunetad: listening on "), "{log}");
    assert_eq!(pid(&watch(&host, Path::new(LUNETA))), daemon.pid);
}

#[test]
fn lunetad_says_in_lunetas_log_when_and_which_daemon() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    let log = fs::read_to_string(host.home.path().join(".local/state/luneta/luneta.log")).unwrap();
    let listening = format!("lunetad[{}] listening on ", daemon.pid());
    let line = log.lines().find(|line| line.contains(&listening)).expect(&log);
    let (when, said) = line.split_once(' ').unwrap();
    assert!(said.starts_with(&listening), "{line}");
    let date = |at: usize| when.as_bytes().get(at).copied();
    assert_eq!((when.len(), date(10), date(23)), (24, Some(b'T'), Some(b'Z')), "{when}");
}

#[test]
fn lunetad_with_no_log_says_first_why_it_does_not_start() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("r".repeat(110));
    fs::create_dir(&runtime).unwrap();
    let (ended, said) = start(Vars::runtime(&runtime)).err().unwrap();
    assert!(!ended.success());
    let socket = runtime.join(SOCKET);
    let first = said.lines().next().unwrap_or_default();
    assert!(first.starts_with(&format!("lunetad: {}: ", socket.display())), "{said}");
}

#[test]
fn the_cli_says_that_no_daemon_runs_and_starts_none() {
    let host = Host::new();
    let none_runs = |host: &Host| {
        for command in ["info", "agents"] {
            let output = answered(host.vars().command(LUNETA).arg(command));
            let said = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(NONE_RUNS), "{command}: {said}");
            let none = format!("luneta: no lunetad runs in {}", host.folder().display());
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
fn lunetas_that_start_a_daemon_at_once_reach_the_same_one() {
    const LUNETAS: usize = 12;
    let host = Host::new();
    let elsewhere = moved();
    let spawned = elsewhere.path().join("spawned");
    let stand_in = format!(
        "echo >> '{spawned}'\n\
         waited=0\n\
         while [ $(wc -l < '{spawned}') -lt {LUNETAS} ] && [ $waited -lt 300 ]; do\n\
         \x20   sleep 0.01\n\
         \x20   waited=$((waited + 1))\n\
         done\n\
         exec '{LUNETAD}' \"$@\"\n",
        spawned = spawned.display()
    );
    script(&elsewhere.path().join("lunetad"), &stand_in);
    let luneta = elsewhere.path().join("luneta");
    let pids: Vec<u32> = thread::scope(|scope| {
        let asking: Vec<_> =
            (0..LUNETAS).map(|_| scope.spawn(|| pid(&watch(&host, &luneta)))).collect();
        asking.into_iter().map(|asked| asked.join().unwrap()).collect()
    });
    let _daemon = Detached { host: &host, pid: pids[0] };
    assert!(pids.iter().all(|pid| *pid == pids[0]), "{pids:?}");
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), LUNETAS);
    let lost = || log(&host).matches("another lunetad already runs").count();
    eventually("the other lunetads lose", || (lost() == LUNETAS - 1).then_some(()));
    let log = log(&host);
    assert_eq!(log.matches("lunetad: listening on ").count(), 1, "{log}");
}

#[test]
fn a_luneta_whose_daemon_finds_the_folder_held_waits_for_it() {
    let host = Host::new();
    let elsewhere = moved();
    let (hold, spawned) = (elsewhere.path().join("hold"), elsewhere.path().join("spawned"));
    let stand_in = format!(
        "echo >> '{}'\nwhile [ -e '{}' ]; do sleep 0.01; done\nexec '{LUNETAD}' \"$@\"\n",
        spawned.display(),
        hold.display()
    );
    script(&elsewhere.path().join("lunetad"), &stand_in);
    fs::write(&hold, "").unwrap();
    let luneta = elsewhere.path().join("luneta");

    let answer = thread::scope(|scope| {
        let asking = scope.spawn(|| watch(&host, &luneta));
        eventually("luneta starts a lunetad", || spawned.exists().then_some(()));
        let claim = Claim::take(&host.folder()).unwrap();
        fs::remove_file(&hold).unwrap();
        eventually("the lunetad finds the folder held", || {
            log(&host).contains("another lunetad already runs").then_some(())
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
        "echo >> '{}'\necho 'lunetad: listening on it' >&2\nkill -TERM $$\n",
        spawned.display()
    );
    script(&elsewhere.path().join("lunetad"), &stand_in);
    spawned
}

#[test]
fn a_luneta_whose_lunetad_is_stopped_as_it_starts_waits_for_the_next() {
    let host = Host::new();
    let elsewhere = moved();
    let spawned = stopped_as_it_starts(&elsewhere);
    let luneta = elsewhere.path().join("luneta");

    thread::scope(|scope| {
        let asking = scope.spawn(|| watch(&host, &luneta));
        eventually("luneta starts a lunetad", || spawned.exists().then_some(()));
        thread::sleep(Duration::from_millis(300));
        let next = start(host.vars()).unwrap();
        assert_eq!(pid(&asking.join().unwrap()), next.pid());
    });
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), 1);
}

#[test]
fn a_luneta_whose_lunetad_is_stopped_as_it_starts_says_it_was_stopped() {
    let host = Host::new();
    let elsewhere = moved();
    let spawned = stopped_as_it_starts(&elsewhere);
    let luneta = elsewhere.path().join("luneta");

    let output = answered(host.vars().command(&luneta.display().to_string()).arg("watch"));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("the lunetad luneta started was stopped"), "{said}");
    assert!(!said.contains("did not start"), "{said}");
    assert_eq!(fs::read_to_string(&spawned).unwrap().lines().count(), 1);
}

#[test]
fn luneta_starts_the_lunetad_beside_it_or_else_the_one_on_the_path() {
    let host = Host::new();
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let stand_in = |name: &str| {
        format!("echo {name} >> '{}'\nexec '{LUNETAD}' \"$@\"\n", started.display())
    };
    script(&elsewhere.path().join("lunetad"), &stand_in("beside"));
    script(&host.bin().join("lunetad"), &stand_in("path"));
    let luneta = elsewhere.path().join("luneta");

    stop(pid(&watch(&host, &luneta)));
    eventually("the daemon beside luneta ends", || (!held(&host)).then_some(()));
    fs::remove_file(elsewhere.path().join("lunetad")).unwrap();
    let _daemon = Detached { host: &host, pid: pid(&watch(&host, &luneta)) };
    assert_eq!(fs::read_to_string(&started).unwrap(), "beside\npath\n");
}

#[test]
fn luneta_watch_starts_a_daemon_of_its_version_in_the_place_of_an_older_one() {
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None);
    let said = info(host.vars());
    assert!(said.starts_with(&format!("lunetad {OLDER} (pid {})", older.pid())), "{said}");

    let elsewhere = moved();
    let other = "if [ \"$1\" = --version ]; then echo 'lunetad 0.0.0'; exit; fi\nexit 1\n";
    let beside = elsewhere.path().join("lunetad");
    script(&beside, other);
    let said = refused(&host, &elsewhere.path().join("luneta"));
    let real = beside.canonicalize().unwrap();
    let kept = format!("but {} is 0.0.0, no newer, so luneta leaves it be", real.display());
    assert!(said.contains(&format!("lunetad {OLDER} (pid {})", older.pid())), "{said}");
    assert!(said.contains(&kept), "{said}");
    assert_eq!(older.ended_within(Duration::ZERO), None);

    let current = watch(&host, Path::new(LUNETA));
    let _daemon = Detached { host: &host, pid: pid(&current) };
    let ended = older.ended_within(Duration::from_secs(5)).expect("the older daemon runs on");
    assert_eq!(ended.signal(), Some(Signal::TERM.as_raw()));
    assert_ne!(pid(&current), older.pid());
    let said = info(host.vars());
    let current = release::spelled(release::VERSION, release::BUILT);
    assert!(said.starts_with(&format!("lunetad {current} ")), "{said}");
}

#[test]
fn luneta_watch_goes_by_the_stamp_of_a_daemon_of_its_release() {
    let stamped = |built| release::spelled(release::VERSION, Some(built));

    let later = Host::new();
    let mut after = stand_in(&later, &stamped(u64::MAX), None);
    let said = refused(&later, Path::new(LUNETA));
    assert!(said.contains(&format!("is newer than this luneta, {}", *release::SAID)), "{said}");
    assert_eq!(after.ended_within(Duration::ZERO), None);

    let host = Host::new();
    let mut before = stand_in(&host, &stamped(0), None);
    if release::BUILT.is_some() {
        let reached = pid(&watch(&host, Path::new(LUNETA)));
        let _daemon = Detached { host: &host, pid: reached };
        assert!(before.ended_within(Duration::from_secs(5)).is_some(), "the stamped daemon runs on");
        assert_ne!(reached, before.pid());
    } else {
        let said = refused(&host, Path::new(LUNETA));
        assert!(said.contains(&format!("lunetad {} (pid ", stamped(0))), "{said}");
        assert_eq!(before.ended_within(Duration::ZERO), None);
    }
}

#[test]
fn luneta_and_lunetad_say_their_build_alike() {
    for (program, name) in [(LUNETA, "luneta"), (LUNETAD, "lunetad")] {
        let said = Command::new(program).arg("--version").output().unwrap();
        assert!(said.status.success(), "{name} --version failed");
        assert_eq!(String::from_utf8_lossy(&said.stdout), format!("{name} {}\n", *release::SAID));
    }
}

#[test]
fn lunetas_that_find_an_older_daemon_at_once_reach_the_one_in_its_place() {
    const LUNETAS: usize = 8;
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None);
    let pids: Vec<u32> = thread::scope(|scope| {
        let asking: Vec<_> = (0..LUNETAS)
            .map(|_| scope.spawn(|| pid(&watch(&host, Path::new(LUNETA)))))
            .collect();
        asking.into_iter().map(|asked| asked.join().unwrap()).collect()
    });
    let _daemon = Detached { host: &host, pid: pids[0] };
    assert!(older.ended_within(Duration::from_secs(5)).is_some(), "the older daemon runs on");
    assert!(pids.iter().all(|pid| *pid == pids[0] && *pid != older.pid()), "{pids:?}");
}

#[test]
fn luneta_watch_refuses_a_daemon_newer_than_it_and_leaves_it_be() {
    let host = Host::new();
    let mut newer = stand_in(&host, NEWER, None);
    let said = refused(&host, Path::new(LUNETA));
    let (pid, own) = (newer.pid(), release::SAID.as_str());
    let both = format!("lunetad {NEWER} (pid {pid}) is newer than this luneta, {own}");
    assert!(said.contains(&both), "{said}");
    assert!(said.contains(&format!("`kill {pid}`")), "{said}");
    assert_eq!(newer.ended_within(Duration::ZERO), None);
}

#[test]
fn the_cli_asks_no_daemon_of_another_build_and_info_says_it_is_one() {
    for version in [OLDER, NEWER] {
        let host = Host::new();
        let mut other = stand_in(&host, version, None);
        let output = answered(host.vars().command(LUNETA).arg("agents"));
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{said}");
        let theirs = format!("lunetad {version} (pid {})", other.pid());
        assert!(said.contains(&theirs), "{said}");
        assert!(said.contains(release::SAID.as_str()), "{said}");

        let output = answered(host.vars().command(LUNETA).arg("info"));
        let out = String::from_utf8_lossy(&output.stdout);
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{said}");
        assert!(out.starts_with(&theirs), "{out}");
        assert!(said.contains(&theirs) && said.contains(release::SAID.as_str()), "{said}");
        assert_eq!(other.ended_within(Duration::ZERO), None);
    }
}

#[test]
fn luneta_watch_stops_no_process_but_the_one_the_kernel_says_listens() {
    let host = Host::new();
    let mut decoy = Running::spawn(Command::new("sleep").arg("60"));
    let mut older = stand_in(&host, OLDER, Some(decoy.pid()));
    let said = refused(&host, Path::new(LUNETA));
    let listens = "but the kernel says another process listens on its socket, so luneta stops none";
    assert!(said.contains(listens), "{said}");
    assert_eq!(decoy.ended_within(Duration::ZERO), None);
    assert_eq!(older.ended_within(Duration::ZERO), None);
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
        .args(["demand::stand_in_lunetad", "--exact", "--ignored"])
        .env(STAND_IN, host.folder())
        .env(STAND_IN_VERSION, version)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[test]
#[ignore = "the stand-in for a daemon of another version, which a test runs as a process"]
fn stand_in_lunetad() {
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
    let test = "demand::stand_in_bridged_lunetad --exact --ignored";
    let to = "3>&1 1>/dev/null 2>/dev/null";
    format!("exec env {STAND_IN_BRIDGED}='{version}' '{}' {test} {to}\n", program.display())
}

#[test]
#[ignore = "the stand-in for a daemon of another version that luneta bridge hands on, which a test \
            runs as a process"]
fn stand_in_bridged_lunetad() {
    let Ok(said) = env::var(STAND_IN_BRIDGED) else {
        return;
    };
    let (version, built) = build_of(said);
    let mut out = OpenOptions::new().write(true).open(BRIDGED_FD).unwrap();
    out.write_all(lunetad::BRIDGED).unwrap();
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

    async fn list_skills(
        &self,
        _: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        Err(Status::unimplemented("a stand-in finds no skills"))
    }

    async fn list_dirs(
        &self,
        _: Request<ListDirsRequest>,
    ) -> Result<Response<ListDirsResponse>, Status> {
        Err(Status::unimplemented("a stand-in knows no directory"))
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

    type FramesStream = tokio_stream::Empty<Result<lunetad::mux::own::api::ToViewer, Status>>;

    async fn frames(
        &self,
        _: Request<Streaming<api::FramesRequest>>,
    ) -> Result<Response<Self::FramesStream>, Status> {
        Err(Status::unimplemented("a stand-in shows no terminal"))
    }
}

#[test]
fn the_waits_of_half_a_minute_hold_side_by_side() {
    let waits: [(&str, fn()); 5] = [
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
    ];
    let failed: Vec<&str> = thread::scope(|scope| {
        let running = waits.map(|(name, wait)| {
            let named = thread::Builder::new().name(name.to_owned());
            (name, named.spawn_scoped(scope, wait).unwrap())
        });
        let ended = running.map(|(name, it)| (name, it.join()));
        ended.into_iter().filter(|(_, ended)| ended.is_err()).map(|(name, _)| name).collect()
    });
    assert_eq!(failed, [] as [&str; 0]);
}

fn the_daemon_ends_once_nothing_has_kept_it_for_a_while() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    host.host();
    let _silent = UnixStream::connect(host.runtime.path().join(SOCKET)).unwrap();
    let alone = Instant::now();
    assert_eq!(daemon.ended_within(Duration::from_secs(25)), None);
    let ended = daemon.ended_within(Duration::from_secs(20)).expect("the daemon runs on");
    let took = alone.elapsed();

    assert!(ended.success(), "{ended}");
    assert!(took >= Duration::from_secs(30), "{took:?}");
    assert!(!host.runtime.path().join(SOCKET).exists());
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(said.iter().any(|line| line.contains("no terminal for 30s")), "{said:?}");
}

fn the_daemon_runs_on_while_a_client_watches_it() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let watched = Instant::now() + Duration::from_secs(36);
    runtime.block_on(async {
        let mut watching = lunetad::connect(&folder).await.unwrap();
        let mut after = None;
        while Instant::now() < watched {
            let asked = watching.watch(WatchRequest { after, kept: None, shown: false }).await;
            after = asked.expect("the daemon ended").into_inner().version;
        }
    });
    assert_eq!(daemon.ended_within(Duration::ZERO), None);
}
