use std::error::Error;
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::process::{Command, Stdio};

use luneta_waits::PATIENCE;
use lunetad::api::WatchRequest;
use lunetad::{Unbridged, BRIDGED};

use crate::demand::Detached;
use crate::host::{folder_in, start, Host, Running, LUNETA};

const OPENING: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00";

const SETTINGS: u8 = 0x4;

fn bridge(host: &Host) -> Command {
    let mut command = host.vars().command(LUNETA);
    command.arg("bridge");
    command
}

fn sh(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

fn went(error: &(dyn Error + 'static)) -> bool {
    let mut sources = std::iter::successors(Some(error), |&error| error.source());
    let went = |error: &io::Error| error.kind() == ErrorKind::NotConnected;
    sources.any(|error| match error.downcast_ref::<Unbridged>() {
        Some(Unbridged::Io(error)) => went(error),
        _ => error.downcast_ref::<io::Error>().is_some_and(went),
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn unbridged(command: Command, is: impl Fn(&Unbridged) -> bool) {
    let error = runtime().block_on(lunetad::through(command)).expect_err("it reached one");
    let first: &(dyn Error + 'static) = &error;
    let mut sources = std::iter::successors(Some(first), |&error| error.source());
    let why = sources.find_map(|error| error.downcast_ref::<Unbridged>());
    assert!(why.is_some_and(is), "{error:?}");
}

#[test]
fn a_client_asks_the_daemon_of_the_folder_through_luneta_bridge() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    runtime().block_on(async {
        let mut bridged = lunetad::through(bridge(&host)).await.unwrap();
        let info = bridged.info(()).await.unwrap().into_inner();
        assert_eq!(info.pid, daemon.pid());
        let first = WatchRequest { after: None, kept: None, shown: false };
        let watched = bridged.watch(first).await.unwrap().into_inner();
        assert_eq!(watched.version.unwrap().pid, daemon.pid());
    });
}

#[test]
fn a_client_runs_its_command_once_and_one_that_stalls_ends_it() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    runtime().block_on(async {
        let mut stalled = lunetad::through(sh("echo lunetad; exec sleep 60")).await.unwrap();
        stalled.info(()).await.expect_err("a stalled command answered");
        let again = stalled.info(()).await.expect_err("it ran a command again");
        assert!(went(&again), "{again:?}");
        let mut bridged = lunetad::through(bridge(&host)).await.unwrap();
        let info = bridged.info(()).await.unwrap().into_inner();
        assert_eq!(info.pid, daemon.pid());
    });
}

#[test]
fn a_client_reaches_no_daemon_but_the_one_it_dialed() {
    let host = Host::new();
    let first = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    runtime().block_on(async {
        let mut daemon = lunetad::connect(&folder, PATIENCE).await.unwrap();
        assert_eq!(daemon.info(()).await.unwrap().into_inner().pid, first.pid());
        drop(first);
        let second = start(host.vars()).unwrap();
        let lost = daemon.info(()).await.expect_err("it reached the second daemon");
        let again = daemon.info(()).await.expect_err("it reached the second daemon");
        assert!(went(&again), "{lost:?}, then {again:?}");
        let mut fresh = lunetad::connect(&folder, PATIENCE).await.unwrap();
        assert_eq!(fresh.info(()).await.unwrap().into_inner().pid, second.pid());
    });
}

#[test]
fn luneta_bridge_starts_a_daemon_where_none_runs() {
    let host = Host::new();
    let pid = runtime().block_on(async {
        let mut bridged = lunetad::through(bridge(&host)).await.unwrap();
        bridged.info(()).await.unwrap().into_inner().pid
    });
    let daemon = Detached { host: &host, pid };

    let folder = folder_in(host.runtime.path()).unwrap();
    let answering = runtime().block_on(async {
        let mut socket = lunetad::connect(&folder, PATIENCE).await.unwrap();
        socket.info(()).await.unwrap().into_inner().pid
    });
    assert_eq!(answering, daemon.pid);
}

#[test]
fn a_client_is_told_why_a_command_reached_no_daemon() {
    unbridged(sh("exit 127"), |why| {
        matches!(why, Unbridged::Ended(ended) if ended.code() == Some(127))
    });
    unbridged(sh("echo hello; exec sleep 10"), |why| {
        matches!(why, Unbridged::Said(said) if said == "hello\n")
    });
    unbridged(sh("exec sleep 60"), |why| matches!(why, Unbridged::Silent));
}

#[test]
fn luneta_bridge_ends_as_either_end_hangs_up() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    let piped = || {
        let mut command = bridge(&host);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        command
    };

    let mut left = Running::spawn(&mut piped());
    let (stdin, _stdout) = left.piped();
    drop(stdin);
    let ended = left.ended("luneta bridge ends with its client");
    assert!(ended.success(), "{ended}");

    let mut bridged = Running::spawn(&mut piped());
    let (mut stdin, mut stdout) = bridged.piped();
    stdin.write_all(OPENING).unwrap();
    let mut said = vec![0; BRIDGED.len()];
    stdout.read_exact(&mut said).unwrap();
    assert_eq!(said, BRIDGED);
    let mut frame = [0; 9];
    stdout.read_exact(&mut frame).unwrap();
    assert_eq!(frame[3], SETTINGS, "{frame:?}");
    drop(daemon);
    let ended = bridged.ended("luneta bridge ends with its daemon");
    assert!(ended.success(), "{ended}");
}
