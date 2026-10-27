//! The daemon on demand: `luneta` starts it where none runs, and in the place
//! of an older one, and it ends once zellij has had no session for a while.

use std::env;
use std::fs;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::daemon_server::{Daemon, DaemonServer};
use lunetad::api::{
    self, InfoRequest, InfoResponse, ListAgentsRequest, ListAgentsResponse, Version, WatchRequest,
};
use lunetad::{folder, Claim};
use prost::Message as _;
use rustix::process::{Pid, Signal};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use crate::host::{
    answered, eventually, folder_in, info, start, Host, Running, LUNETA, LUNETAD, SOCKET,
};

/// The version of the stand-in for a daemon an upgrade left running.
const OLDER: &str = "0.0.1";

/// The version of the stand-in for a daemon that a luneta from before an
/// upgrade finds running.
const NEWER: &str = "99.0.0";

/// Set, it has the test binary, run again, stand in for a daemon in the
/// folder it names, of the version that `STAND_IN_VERSION` says.
const STAND_IN: &str = "LUNETA_TEST_STAND_IN_FOLDER";

const STAND_IN_VERSION: &str = "LUNETA_TEST_STAND_IN_VERSION";

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
    let folder = folder_in(host.runtime.path())?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.ok()?;
        Some(daemon.info(InfoRequest {}).await.ok()?.into_inner().pid)
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
    let mut older = stand_in(&host, OLDER);
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
    assert!(said.starts_with(&format!("lunetad {} ", env!("CARGO_PKG_VERSION"))), "{said}");
}

/// A daemon newer than luneta, as a luneta from before an upgrade finds
/// running, `luneta watch` keeps: were it to stop it, the newer luneta would
/// stop the one it started in turn, and the two would take turns at it for
/// as long as both ask.
#[test]
fn luneta_watch_keeps_a_daemon_newer_than_it() {
    let host = Host::new();
    let mut newer = stand_in(&host, NEWER);
    assert_eq!(pid(&watch(&host, Path::new(LUNETA))), newer.pid());
    assert_eq!(newer.ended_within(Duration::ZERO), None);
}

/// Starts the stand-in for a daemon of `version` in the host's folder, and
/// waits for it to answer.
fn stand_in(host: &Host, version: &str) -> Running {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["demand::stand_in_lunetad", "--exact", "--ignored"])
        .env(STAND_IN, host.folder())
        .env(STAND_IN_VERSION, version)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let stand_in = Running::spawn(&mut command);
    eventually("the stand-in answers", || (answering(host)? == stand_in.pid()).then_some(()));
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
    let version = env::var(STAND_IN_VERSION).unwrap();
    let claim = Claim::take(Path::new(&folder)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::bind(claim.socket()).unwrap();
        tonic::transport::Server::builder()
            .add_service(DaemonServer::new(StandIn { version }))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });
}

struct StandIn {
    version: String,
}

#[tonic::async_trait]
impl Daemon for StandIn {
    async fn info(&self, _: Request<InfoRequest>) -> Result<Response<InfoResponse>, Status> {
        let (version, pid) = (self.version.clone(), std::process::id());
        Ok(Response::new(InfoResponse { version, pid, started_at: None }))
    }

    async fn list_agents(
        &self,
        _: Request<ListAgentsRequest>,
    ) -> Result<Response<ListAgentsResponse>, Status> {
        Ok(Response::new(ListAgentsResponse::default()))
    }

    async fn watch(&self, _: Request<WatchRequest>) -> Result<Response<api::Host>, Status> {
        let version = Version { pid: std::process::id(), seq: 0 };
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
