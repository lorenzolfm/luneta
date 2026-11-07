//! `luneta bridge`: the daemon of its folder for a client on another host, on
//! ssh's stdin and stdout, which the client asks as it would on the socket.

use std::error::Error;
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::Duration;

use lunetad::api::WatchRequest;
use lunetad::{Unbridged, BRIDGED};

use crate::demand::Detached;
use crate::host::{folder_in, start, Host, Running, Vars, LUNETA};

/// How HTTP/2 opens a connection, as a client of the daemon's opens it: the
/// preface, and settings that change none.
const OPENING: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00";

/// The type of an HTTP/2 frame of settings, the first a server sends.
const SETTINGS: u8 = 0x4;

/// What makes a `luneta bridge` to the host's folder, as a client makes one
/// for each connection.
fn bridge(host: &Host) -> impl FnMut() -> Command + Send + 'static {
    let runtime = host.runtime.path().to_owned();
    let home = host.home.path().to_owned();
    let sockets = host.sockets.path().to_owned();
    let bin = host.zellij.bin().to_owned();
    move || {
        let vars = Vars {
            runtime: Some(&runtime),
            home: Some(&home),
            zellij: Some(&sockets),
            bin: Some(&bin),
            ..Vars::default()
        };
        let mut command = vars.command(LUNETA);
        command.arg("bridge");
        command
    }
}

/// A command that runs `script` in `sh`, as a client makes one for each
/// connection.
fn sh(script: &'static str) -> impl FnMut() -> Command + Send + 'static {
    move || {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Asserts that `through` reaches no daemon with `command`, for a reason that
/// `is`, among the sources of the error it fails with.
fn unbridged(command: impl FnMut() -> Command + Send + 'static, is: impl Fn(&Unbridged) -> bool) {
    let error = runtime().block_on(lunetad::through(command)).expect_err("it reached one");
    let first: &(dyn Error + 'static) = &error;
    let mut sources = std::iter::successors(Some(first), |&error| error.source());
    let why = sources.find_map(|error| error.downcast_ref::<Unbridged>());
    assert!(why.is_some_and(is), "{error:?}");
}

/// A client that asks through `luneta bridge` asks the daemon that runs in
/// its folder, question after question on the one connection, `Watch` among
/// them.
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

/// A client through a command that stalls, as ssh's does over a link that
/// dropped without a word, closes the connection once the daemon answers no
/// ping on it, and asks the next question through a command made afresh: a
/// question that times out would leave it open, for the next to time out on.
#[test]
fn a_client_makes_a_command_afresh_for_one_that_stalls() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    // The first says it reached the daemon, and hands on nothing.
    let mut stalled = Some(sh("echo lunetad; exec sleep 60"));
    let mut bridge = bridge(&host);
    let commands = move || match stalled.take() {
        Some(mut stalled) => stalled(),
        None => bridge(),
    };
    runtime().block_on(async {
        let mut bridged = lunetad::through(commands).await.unwrap();
        bridged.info(()).await.expect_err("a stalled command answered");
        let info = bridged.info(()).await.unwrap().into_inner();
        assert_eq!(info.pid, daemon.pid());
    });
}

/// `luneta bridge` starts a daemon where none runs, as `luneta watch` does,
/// so that a client on another host reaches one whether or not a luneta
/// there started it: the one that then answers on the socket.
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
        let mut socket = lunetad::connect(&folder).await.unwrap();
        socket.info(()).await.unwrap().into_inner().pid
    });
    assert_eq!(answering, daemon.pid);
}

/// A client through a command that reaches no daemon is told why: that it
/// ended first, as a shell with no luneta does, what it said first, as a
/// login shell that prints does, or that it said nothing in time, as ssh
/// does that waits on a host that does not answer.
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

/// `luneta bridge` ends as either end hangs up: the client, as its stdin
/// ends, and the daemon, as its socket does. Until then it hands on what each
/// says, here the settings the daemon answers a client's opening with, after
/// it says it reached the daemon.
#[test]
fn luneta_bridge_ends_as_either_end_hangs_up() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    let piped = || {
        let mut command = bridge(&host)();
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        command
    };

    let mut left = Running::spawn(&mut piped());
    let (stdin, _stdout) = left.piped();
    drop(stdin);
    let ended = left.ended_within(Duration::from_secs(5)).expect("it outlived its client");
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
    let ended = bridged.ended_within(Duration::from_secs(5)).expect("it outlived its daemon");
    assert!(ended.success(), "{ended}");
}
