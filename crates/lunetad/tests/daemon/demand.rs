//! The daemon on demand: `luneta` starts it where none runs, and in the place
//! of an older one, and it ends once zellij has had no session for a while.

use std::env;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::daemon_server::{Daemon, DaemonServer};
use lunetad::api::{
    self, InfoRequest, InfoResponse, ListAgentsRequest, ListAgentsResponse, Version, WatchRequest,
};
use lunetad::{folder, release, Claim};
use prost::Message as _;
use rustix::process::{Pid, Signal};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use crate::host::{
    answered, eventually, folder_in, info, start, Host, Running, Vars, LUNETA, LUNETAD, SOCKET,
};

/// The version of the stand-in for a daemon an upgrade left running.
const OLDER: &str = "0.0.1";

/// The version of the stand-in for a daemon that a luneta from before an
/// upgrade finds running.
const NEWER: &str = "99.0.0";

/// Set, it has the test binary, run again, stand in for a daemon in the
/// folder it names, of the build that `STAND_IN_VERSION` spells, as
/// `lunetad --version` says one.
const STAND_IN: &str = "LUNETA_TEST_STAND_IN_FOLDER";

const STAND_IN_VERSION: &str = "LUNETA_TEST_STAND_IN_VERSION";

/// Set, the stand-in says that this is its pid, as a daemon in another
/// namespace of pids says one that is another process's here.
const STAND_IN_PID: &str = "LUNETA_TEST_STAND_IN_PID";

/// Set, the stand-in says that it reads zellij's sessions in this folder, and
/// else nothing of it, as a daemon older than the field.
const STAND_IN_READS: &str = "LUNETA_TEST_STAND_IN_READS";

/// `luneta watch`, run as `luneta`, and the host it wrote out.
fn watch(host: &Host, luneta: &Path) -> api::Host {
    written(host.vars().command(&luneta.display().to_string()).arg("watch"))
}

/// The host that `command`, a `luneta watch`, wrote out.
fn written(command: &mut Command) -> api::Host {
    let output = answered(command);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "luneta watch failed: {said}");
    api::Host::decode(output.stdout.as_slice()).unwrap()
}

/// The pid of the daemon that wrote out `host`.
fn pid(host: &api::Host) -> u32 {
    host.version.unwrap().pid
}

/// The pid of the daemon that answers in the host's folder, if one does.
fn answering(host: &Host) -> Option<u32> {
    told(host).map(|info| info.pid)
}

/// What the daemon that answers in the host's folder says of itself, if one
/// does.
fn told(host: &Host) -> Option<InfoResponse> {
    let folder = folder_in(host.runtime.path())?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.ok()?;
        Some(daemon.info(InfoRequest {}).await.ok()?.into_inner())
    })
}

/// Whether a daemon holds the host's folder, which none does that is not
/// there.
fn held(host: &Host) -> bool {
    folder_in(host.runtime.path()).is_some_and(|folder| folder::held(&folder).unwrap())
}

fn stop(pid: u32) {
    let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::kill_process(pid, Signal::TERM).unwrap();
}

/// What the daemons that luneta started wrote in their log.
fn log(host: &Host) -> String {
    fs::read_to_string(host.folder().join(folder::LOG)).unwrap()
}

/// Puts a program at `to`, a copy of `from`. A process of its own writes it,
/// so that the test's never holds it open to write: a process that another
/// test starts meanwhile would take that along until it runs its program,
/// and until then the copy would not run, as its file is busy.
fn install(from: &Path, to: &Path) {
    let mut install = Command::new("install");
    let installed = install.args(["-m", "755"]).arg(from).arg(to).status().unwrap();
    assert!(installed.success(), "{} was not installed at {}", from.display(), to.display());
}

/// Writes a shell script at `path`.
fn script(path: &Path, text: &str) {
    let draft = path.with_extension("draft");
    fs::write(&draft, format!("#!/bin/sh\n{text}")).unwrap();
    install(&draft, path);
}

/// A copy of `luneta` in a folder of its own, where nothing is beside it.
fn moved() -> tempfile::TempDir {
    let elsewhere = tempfile::tempdir().unwrap();
    install(Path::new(LUNETA), &elsewhere.path().join("luneta"));
    elsewhere
}

/// A daemon that luneta started, which is no child of the test's, and so is
/// stopped by its pid once the test is done with it: if it is still the one
/// that answers in the folder, as a pid that has ended can be another
/// process's by then. Left alone, it would end on its own, as the test's
/// zellij has no session.
struct Detached<'a> {
    host: &'a Host,
    pid: u32,
}

impl Drop for Detached<'_> {
    fn drop(&mut self) {
        if answering(self.host) == Some(self.pid) {
            stop(self.pid);
        }
    }
}

/// `luneta watch` starts lunetad where none runs, and answers once it does.
/// The daemon holds nothing of luneta's: it leads a session of its own, and
/// what it says goes to a log in its folder, so that whoever reads what
/// luneta writes to its end, as zellij does, has its answer. Nor does it
/// hold what luneta was handed open past its stdio, as zellij hands it each
/// pane's terminal: here a second end of luneta's stdout, which would keep
/// that open too. The next `luneta watch` reaches the same daemon.
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

/// What lunetad says goes in luneta's log too, in the state folder of the
/// home, each line with when it said it and its pid: by the time it says on
/// stderr that it listens, the log has it.
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

/// `luneta info`, `agents` and `sessions` say that no daemon runs, as `tmux
/// ls` says that no server does, and start none: in a folder no daemon has
/// made yet, and in one that a daemon that crashed left its socket in.
#[test]
fn the_cli_says_that_no_daemon_runs_and_starts_none() {
    let host = Host::new();
    let none_runs = |host: &Host| {
        for command in ["info", "agents", "sessions"] {
            let output = answered(host.vars().command(LUNETA).arg(command));
            let said = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{command}: {said}");
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

/// lunetas that find no daemon in the same moment start one each, and the
/// folder's lock lets one through: each luneta answers, from that one. The
/// lunetad they start waits until each luneta has started one, or for 3s at
/// the most, so that each finds none.
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
    // The rest lose in their own time, which can be after their lunetas have
    // answered from the one that listens.
    let lost = || log(&host).matches("another lunetad already runs").count();
    eventually("the other lunetads lose", || (lost() == LUNETAS - 1).then_some(()));
    let log = log(&host);
    assert_eq!(log.matches("lunetad: listening on ").count(), 1, "{log}");
}

/// A luneta whose lunetad ends as it finds the folder held, as one that
/// another luneta started in the same moment holds it, waits: for the one
/// that holds it to answer, or to let it go, and then starts another. Here
/// the test holds the folder, with no socket, until the lunetad has ended.
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

/// luneta starts the lunetad beside it, which the same build made, rather
/// than the first on the `PATH`, which can be of another. With none beside
/// it, it starts that one.
#[test]
fn luneta_starts_the_lunetad_beside_it_or_else_the_one_on_the_path() {
    let host = Host::new();
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let stand_in = |name: &str| {
        format!("echo {name} >> '{}'\nexec '{LUNETAD}' \"$@\"\n", started.display())
    };
    script(&elsewhere.path().join("lunetad"), &stand_in("beside"));
    script(&host.zellij.bin().join("lunetad"), &stand_in("path"));
    let luneta = elsewhere.path().join("luneta");

    stop(pid(&watch(&host, &luneta)));
    eventually("the daemon beside luneta ends", || (!held(&host)).then_some(()));
    fs::remove_file(elsewhere.path().join("lunetad")).unwrap();
    let _daemon = Detached { host: &host, pid: pid(&watch(&host, &luneta)) };
    assert_eq!(fs::read_to_string(&started).unwrap(), "beside\npath\n");
}

/// A daemon older than luneta, as an upgrade leaves running, `luneta watch`
/// stops, and starts one of its own version in its place. It does so only
/// where the lunetad it starts is newer than that one: one that is not would
/// be stopped in its turn by the next luneta of the daemon's version, for
/// good. `luneta info` says which runs, and stops none.
#[test]
fn luneta_watch_starts_a_daemon_of_its_version_in_the_place_of_an_older_one() {
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None, None);
    let said = info(host.vars());
    assert!(said.starts_with(&format!("lunetad {OLDER} (pid {})", older.pid())), "{said}");

    let elsewhere = moved();
    let other = "if [ \"$1\" = --version ]; then echo 'lunetad 0.0.0'; exit; fi\nexit 1\n";
    script(&elsewhere.path().join("lunetad"), other);
    assert_eq!(pid(&watch(&host, &elsewhere.path().join("luneta"))), older.pid());
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

/// A daemon of luneta's own release, as one of every commit until the next
/// is, goes by when its commit was made, where the flake stamped it. One
/// stamped after luneta, as a luneta from before an install finds running,
/// it keeps. A stamped luneta stops one stamped before it, as an install
/// does the daemon the install before it left running. A luneta with no
/// stamp, as a cargo build of a checkout is, keeps that one too: an install
/// is newer than any build of a checkout of its release. The flake's build
/// of a commit's tests is stamped and a cargo run of them is not, and CI
/// runs both.
#[test]
fn luneta_watch_goes_by_the_stamp_of_a_daemon_of_its_release() {
    let stamped = |built| release::spelled(release::VERSION, Some(built));

    let later = Host::new();
    let mut after = stand_in(&later, &stamped(u64::MAX), None, None);
    assert_eq!(pid(&watch(&later, Path::new(LUNETA))), after.pid());
    assert_eq!(after.ended_within(Duration::ZERO), None);

    let host = Host::new();
    let mut before = stand_in(&host, &stamped(0), None, None);
    let reached = pid(&watch(&host, Path::new(LUNETA)));
    if release::BUILT.is_some() {
        let _daemon = Detached { host: &host, pid: reached };
        assert!(before.ended_within(Duration::from_secs(5)).is_some(), "the stamped daemon runs on");
        assert_ne!(reached, before.pid());
    } else {
        assert_eq!(reached, before.pid());
        assert_eq!(before.ended_within(Duration::ZERO), None);
    }
}

/// luneta says its build to `--version` as lunetad does, stamp and all, so
/// that either says which install it is of.
#[test]
fn luneta_and_lunetad_say_their_build_alike() {
    for (program, name) in [(LUNETA, "luneta"), (LUNETAD, "lunetad")] {
        let said = Command::new(program).arg("--version").output().unwrap();
        assert!(said.status.success(), "{name} --version failed");
        assert_eq!(String::from_utf8_lossy(&said.stdout), format!("{name} {}\n", *release::SAID));
    }
}

/// lunetas that find an older daemon in the same moment, as the sidebars of
/// an upgrade do as the host changes, each answer from the one that takes its
/// place. One that finds the older one gone since it answered, as another
/// luneta stopped it, waits for the next, and does not ask the one that ended.
#[test]
fn lunetas_that_find_an_older_daemon_at_once_reach_the_one_in_its_place() {
    const LUNETAS: usize = 8;
    let host = Host::new();
    let mut older = stand_in(&host, OLDER, None, None);
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

/// A daemon newer than luneta, as a luneta from before an upgrade finds
/// running, `luneta watch` keeps: were it to stop it, the newer luneta would
/// stop the one it started in turn, and the two would take turns at it for
/// as long as both ask.
#[test]
fn luneta_watch_keeps_a_daemon_newer_than_it() {
    let host = Host::new();
    let mut newer = stand_in(&host, NEWER, None, None);
    assert_eq!(pid(&watch(&host, Path::new(LUNETA))), newer.pid());
    assert_eq!(newer.ended_within(Duration::ZERO), None);
}

/// A daemon older than luneta whose pid is not the one the kernel says
/// listens, as one in another namespace of pids says a pid that is another
/// process's here, `luneta watch` keeps, and stops neither: it stops no
/// process on a daemon's word alone.
#[test]
fn luneta_watch_stops_no_process_but_the_one_the_kernel_says_listens() {
    let host = Host::new();
    let mut decoy = Running::spawn(Command::new("sleep").arg("60"));
    let mut older = stand_in(&host, OLDER, Some(decoy.pid()), None);
    assert_eq!(pid(&watch(&host, Path::new(LUNETA))), decoy.pid());
    assert_eq!(decoy.ended_within(Duration::ZERO), None);
    assert_eq!(older.ended_within(Duration::ZERO), None);
}

/// The daemon says which folder it reads zellij's sessions in: the one its
/// environment names, as luneta's names the one its zellij keeps them in.
#[test]
fn the_daemon_says_the_folder_it_reads_zellij_s_sessions_in() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let reads = told(&host).and_then(|info| info.zellij_folder);
    assert_eq!(reads.as_deref(), Some(host.sockets.path().as_os_str().as_bytes()));
}

/// The daemon says which folder it reads Claude Code's sessions in, where the
/// transcripts of the agents it lists are: the one its environment names.
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

/// A daemon that reads zellij's sessions in another folder than luneta's
/// zellij keeps them in, as one started with another `ZELLIJ_SOCKET_DIR`
/// does, `luneta watch` neither asks nor stops, older though it is, and says
/// why: one started in its place would leave out the sessions of every
/// luneta that reaches it. One that reads them by another path to luneta's
/// folder, as a link, it asks.
#[test]
fn luneta_watch_neither_asks_nor_stops_a_daemon_of_another_zellij_folder() {
    let host = Host::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let mut older = stand_in(&host, OLDER, None, Some(elsewhere.path()));
    let output = answered(host.vars().command(LUNETA).arg("watch"));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    let (reads, own) = (elsewhere.path().display(), host.sockets.path().display());
    assert!(said.contains(&format!("reads zellij's sessions in {reads}, not in {own}")), "{said}");
    assert_eq!(older.ended_within(Duration::ZERO), None);

    let host = Host::new();
    let link = elsewhere.path().join("sockets");
    symlink(host.sockets.path(), &link).unwrap();
    let newer = stand_in(&host, NEWER, None, Some(&link));
    assert_eq!(pid(&watch(&host, Path::new(LUNETA))), newer.pid());
}

/// Starts the stand-in for a daemon of the build `version` spells in the
/// host's folder, and waits for it to answer. It says that `says` is its pid,
/// and that it reads zellij's sessions in `reads`, if given.
fn stand_in(host: &Host, version: &str, says: Option<u32>, reads: Option<&Path>) -> Running {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["demand::stand_in_lunetad", "--exact", "--ignored"])
        .env(STAND_IN, host.folder())
        .env(STAND_IN_VERSION, version)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(pid) = says {
        command.env(STAND_IN_PID, pid.to_string());
    }
    if let Some(reads) = reads {
        command.env(STAND_IN_READS, reads);
    }
    let stand_in = Running::spawn(&mut command);
    let said = says.unwrap_or(stand_in.pid());
    eventually("the stand-in answers", || (answering(host)? == said).then_some(()));
    stand_in
}

/// A daemon of another version, as the test binary that `stand_in` runs
/// again: it holds the folder and answers there as one would, and ends as
/// SIGTERM ends it. Run on its own, as the tests are, it does nothing.
#[test]
#[ignore = "the stand-in for a daemon of another version, which a test runs as a process"]
fn stand_in_lunetad() {
    let Some(folder) = env::var_os(STAND_IN) else {
        return;
    };
    let said = env::var(STAND_IN_VERSION).unwrap();
    let (version, built) = match said.split_once('+') {
        Some((version, built)) => (version.to_owned(), Some(built.parse().unwrap())),
        None => (said, None),
    };
    let pid = env::var(STAND_IN_PID).map_or(std::process::id(), |pid| pid.parse().unwrap());
    let reads = env::var_os(STAND_IN_READS).map(PathBuf::from);
    let claim = Claim::take(Path::new(&folder)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::bind(claim.socket()).unwrap();
        tonic::transport::Server::builder()
            .add_service(DaemonServer::new(StandIn { version, built, pid, reads }))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });
}

struct StandIn {
    version: String,
    /// The stamp it says it was built with, if it says one.
    built: Option<u64>,
    /// The pid it says it has.
    pid: u32,
    /// The folder it says it reads zellij's sessions in, if it says one.
    reads: Option<PathBuf>,
}

#[tonic::async_trait]
impl Daemon for StandIn {
    async fn info(&self, _: Request<InfoRequest>) -> Result<Response<InfoResponse>, Status> {
        let (version, built, pid) = (self.version.clone(), self.built, self.pid);
        let zellij_folder = self.reads.as_ref().map(|reads| reads.as_os_str().as_bytes().to_vec());
        let info = InfoResponse { version, built, pid, zellij_folder, ..InfoResponse::default() };
        Ok(Response::new(info))
    }

    async fn list_agents(
        &self,
        _: Request<ListAgentsRequest>,
    ) -> Result<Response<ListAgentsResponse>, Status> {
        Ok(Response::new(ListAgentsResponse::default()))
    }

    async fn watch(&self, _: Request<WatchRequest>) -> Result<Response<api::Host>, Status> {
        let version = Version { pid: self.pid, seq: 0 };
        Ok(Response::new(api::Host { version: Some(version), ..api::Host::default() }))
    }
}

/// The daemon ends once zellij has had no session for 30 seconds, and not
/// before. It ends well, says why, and takes its socket with it. A client
/// that holds a connection and never says anything does not keep it: the
/// daemon waits for its clients to go for 5 seconds at the most. The test
/// waits it out.
#[test]
fn the_daemon_ends_once_zellij_has_had_no_session_for_a_while() {
    let host = Host::new();
    let work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
    eventually("work runs", || (!host.host().sessions.is_empty()).then_some(()));
    let _silent = UnixStream::connect(host.runtime.path().join(SOCKET)).unwrap();

    drop(work);
    fs::remove_file(host.contract().join("work")).unwrap();
    let alone = Instant::now();
    eventually("work has ended", || host.host().sessions.is_empty().then_some(()));
    assert_eq!(daemon.ended_within(Duration::from_secs(25)), None);
    let ended = daemon.ended_within(Duration::from_secs(20)).expect("the daemon runs on");
    let took = alone.elapsed();

    assert!(ended.success(), "{ended}");
    assert!(took >= Duration::from_secs(30), "{took:?}");
    assert!(!host.runtime.path().join(SOCKET).exists());
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(said.iter().any(|line| line.contains("no session for 30s")), "{said:?}");
}
