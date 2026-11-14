use std::fs::{self, File};
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lunetad::program::alone;
use rustix::fs::{Mode, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};
use serde_json::json;
use tempfile::TempDir;

use crate::demand::{answering, moved, stand_in_watched_as, stop, Detached};
use crate::host::{eventually, script, start, Host, Running, Vars, LUNETA};
use crate::remote::{hosts, FakeSsh};
use crate::sessions::listed;

const ENDS: Duration = Duration::from_secs(30);

const REACH: Duration = Duration::from_secs(10);

const HEARING: Duration = Duration::from_secs(5);

pub struct Ended {
    pub status: ExitStatus,
    pub said: String,
}

pub fn terminal() -> (OwnedFd, OwnedFd) {
    let controller = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&controller, FdFlags::CLOEXEC).unwrap();
    pty::grantpt(&controller).unwrap();
    pty::unlockpt(&controller).unwrap();
    let path = pty::ptsname(&controller, Vec::new()).unwrap().into_string().unwrap();
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    let tty: OwnedFd = rustix::fs::open(&path, flags, Mode::empty()).unwrap();
    let size = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    termios::tcsetwinsize(&tty, size).unwrap();
    (controller, tty)
}

pub fn spawn_on(tty: &OwnedFd, command: &mut Command) -> Running {
    let stdio = || Stdio::from(tty.try_clone().unwrap());
    Running::spawn(alone(command).stdin(stdio()).stdout(stdio()).stderr(stdio()))
}

pub fn on_terminal(command: Command) -> Ended {
    on_terminal_with(command, |_| {})
}

fn on_terminal_with(mut command: Command, meanwhile: impl FnOnce(&Running)) -> Ended {
    let (controller, tty) = terminal();
    let reader = thread::spawn(move || {
        let mut said = Vec::new();
        let _ = File::from(controller).read_to_end(&mut said);
        said
    });
    let mut luneta = spawn_on(&tty, &mut command);
    drop(command);
    drop(tty);
    meanwhile(&luneta);
    let status = luneta.ended_within(ENDS).expect("luneta did not end");
    let said = reader.join().unwrap();
    Ended { status, said: String::from_utf8_lossy(&said).into_owned() }
}

pub struct Directory {
    _parent: TempDir,
    pub path: PathBuf,
}

pub fn directory(name: &str) -> Directory {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join(name);
    fs::create_dir(&path).unwrap();
    Directory { _parent: parent, path }
}

pub fn luneta(vars: Vars, dir: &Path) -> Command {
    luneta_at(LUNETA, vars, dir)
}

fn luneta_at(program: &str, vars: Vars, dir: &Path) -> Command {
    let mut command = vars.command(program);
    command.current_dir(dir).env_remove("SSH_CONNECTION").env_remove("SSH_TTY");
    command
}

pub fn runs(host: &Host, name: &str) -> std::os::unix::net::UnixListener {
    host.zellij.list(name, &json!([]));
    host.serve(name)
}

#[test]
fn a_bare_luneta_joins_the_session_of_its_directory_or_else_the_newest() {
    let host = Host::new();
    let _older = runs(&host, "project");
    thread::sleep(Duration::from_millis(20));
    let _newest = runs(&host, "newest");

    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    let ended = on_terminal(luneta(host.vars(), &directory("elsewhere").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach project", "attach newest"]);
}

#[test]
fn a_bare_luneta_passes_over_a_session_zellij_does_not_attach_to() {
    let host = Host::new();
    let _older = runs(&host, "older");
    thread::sleep(Duration::from_millis(20));
    let _dashed = runs(&host, "-c");

    let ended = on_terminal(luneta(host.vars(), &directory("elsewhere").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach older"]);
}

#[test]
fn a_bare_luneta_joins_the_session_you_were_in_last() {
    let host = Host::new();
    let _seen = runs(&host, "seen");
    thread::sleep(Duration::from_millis(20));
    let _newest = runs(&host, "newest");
    let daemon = start(host.vars()).unwrap();
    let listed = host.host().sessions;
    let seen = listed.iter().find(|session| session.name == "seen").unwrap();
    let started = seen.started_at.unwrap();
    let nanos = |seconds: i64, nanos: i32| i128::from(seconds) * 1_000_000_000 + i128::from(nanos);
    let session = nanos(started.seconds, started.nanos);
    let ledger = host.folder().join("been");
    fs::create_dir_all(&ledger).unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    fs::write(ledger.join(format!("{session}-{}-1", seen.socket)), format!("{now}\n")).unwrap();
    drop(daemon);

    let ended = on_terminal(luneta(host.vars(), &directory("elsewhere").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach seen"]);
}

#[test]
fn a_bare_luneta_makes_a_session_only_where_none_runs() {
    let host = Host::new();
    host.keep("ended");
    let _gone = host.serve("gone");
    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach --create project"]);
}

#[test]
fn a_session_named_is_made_though_another_runs() {
    let host = Host::new();
    let _other = runs(&host, "other");
    let dir = directory("project");
    let mut named = luneta(host.vars(), &dir.path);
    named.arg("named");
    let ended = on_terminal(named);
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached()[0], "attach --create named");
}

#[test]
fn a_bare_luneta_never_joins_the_session_its_terminal_is_a_pane_of() {
    let host = Host::new();
    let _project = runs(&host, "project");
    let dir = directory("project");
    let mut alone = luneta(host.vars(), &dir.path);
    let output = alone.env("ZELLIJ_SESSION_NAME", "project").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    assert!(refusal.starts_with("luneta: this terminal is a pane of \"project\""), "{refusal}");
    assert_eq!(host.zellij.attached(), [] as [String; 0]);

    let _other = runs(&host, "other");
    let mut other = luneta(host.vars(), &dir.path);
    other.env("ZELLIJ_SESSION_NAME", "project");
    let ended = on_terminal(other);
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach other"]);
}

#[test]
fn a_bare_luneta_never_joins_its_own_session_once_that_is_renamed() {
    let host = Host::new();
    let dir = directory("project");
    let in_a_pane = || {
        let mut luneta = luneta(host.vars(), &dir.path);
        luneta.env("ZELLIJ_SESSION_NAME", "opened-as").env("ZELLIJ_PANE_ID", "1");
        luneta
    };
    let _renamed = host.serve("renamed");
    host.zellij.list("renamed", &pane_running("luneta"));
    let _untold = host.serve("untold");
    let mut untold = pane_running("");
    untold[0]["pane_command"] = json!(null);
    host.zellij.list("untold", &untold);

    let output = in_a_pane().output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    assert!(refusal.contains("this terminal is a pane of"), "{refusal}");
    assert!(refusal.contains("\"renamed\"") && refusal.contains("\"untold\""), "{refusal}");
    assert_eq!(host.zellij.attached(), [] as [String; 0]);

    let _other = host.serve("other");
    host.zellij.list("other", &pane_running("fish"));
    let ended = on_terminal(in_a_pane());
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach other"]);
}

#[test]
fn luneta_never_goes_on_in_its_own_session_once_that_is_renamed() {
    let host = Host::new();
    let _renamed = host.serve("renamed");
    host.zellij.list("renamed", &pane_running("luneta"));
    let dir = directory("project");
    let mut named = luneta(host.vars(), &dir.path);
    named.arg("named").env("ZELLIJ_SESSION_NAME", "opened-as").env("ZELLIJ_PANE_ID", "1");

    let ended = on_terminal(named);
    assert_eq!(host.zellij.attached(), ["attach --create named"]);
    assert!(ended.status.success(), "{}", ended.said);
}

#[test]
fn a_bare_luneta_says_where_the_hosts_file_does_not_read() {
    let host = Host::new();
    fs::create_dir_all(host.home.path().join(".local/state/luneta/hosts")).unwrap();
    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert!(ended.said.contains("luneta/hosts: "), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach --create project"]);
}

fn log_of(host: &Host) -> String {
    fs::read_to_string(host.home.path().join(".local/state/luneta/luneta.log")).unwrap()
}

#[test]
fn a_client_zellij_drops_is_taken_back_to_its_session() {
    let host = Host::new();
    let _session = runs(&host, "project");
    host.zellij.drops_clients(1);

    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach project", "attach project"]);
    let log = log_of(&host);
    assert!(log.contains("zellij dropped its client: taking it back to project"), "{log}");
}

#[test]
fn a_client_zellij_keeps_dropping_at_once_is_given_up_on() {
    let host = Host::new();
    let _session = runs(&host, "project");
    host.zellij.drops_clients(100);

    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach project"; 6]);
    assert!(ended.said.contains("zellij dropped its client 6 times"), "{}", ended.said);
    let log = log_of(&host);
    assert!(log.contains("ends: zellij dropped its client 6 times"), "{log}");
}

pub fn a_client_that_lasted_starts_the_count_of_those_zellij_drops_again() {
    let host = Host::new();
    let _project = runs(&host, "project");
    let _next = runs(&host, "next");
    let lasts = Duration::from_secs(31);
    host.zellij.drops_clients(5);
    host.zellij.stays_for(lasts);

    let ended = on_terminal_with(luneta(host.vars(), &directory("project").path), |_| {
        eventually("a client to stay", || host.zellij.stayed().then_some(()));
        host.zellij.drops_clients(1);
        fs::remove_file(host.contract().join("project")).unwrap();
        thread::sleep(lasts);
    });
    assert!(ended.status.success(), "{}", ended.said);
    let mut attached = vec!["attach project"; 6];
    attached.extend(["attach next"; 2]);
    assert_eq!(host.zellij.attached(), attached);
}

#[test]
fn a_client_that_took_the_place_of_another_ends_the_asking_after_it() {
    let host = Host::new();
    let _project = runs(&host, "project");
    let _next = runs(&host, "next");
    host.zellij.stays_unlisted_for(Duration::from_secs(1));

    on_terminal_with(luneta(host.vars(), &directory("project").path), |luneta| {
        eventually("a client to stay", || host.zellij.stayed().then_some(()));
        host.zellij.listen();
        fs::remove_file(host.contract().join("project")).unwrap();
        eventually("another client", || (host.zellij.attached().len() == 2).then_some(()));
        thread::sleep(Duration::from_secs(1));
        let asked = host.zellij.asked_clients("project");
        assert!(asked > 0, "luneta never asked after its client");
        thread::sleep(Duration::from_millis(500));
        assert_eq!(host.zellij.asked_clients("project"), asked);
        stop(luneta.pid());
    });
}

#[test]
fn a_client_zellij_drops_on_another_machine_is_taken_back_there() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "work", &json!([]));
    ssh.drops_clients("far", 1);
    hosts(&here, "far\n");
    let _daemon = start(here.vars()).unwrap();

    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("far"), ["zellij 'attach' 'work'", "zellij 'attach' 'work'"]);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);
    let log = log_of(&here);
    assert!(log.contains("far: zellij dropped its client: taking it back to work"), "{log}");
}

#[test]
fn the_log_says_why_luneta_ended() {
    let host = Host::new();
    let _session = runs(&host, "project");
    let detached = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(detached.status.success(), "{}", detached.said);
    let log = log_of(&host);
    assert!(log.contains("ends: zellij's client ended, and its session runs on"), "{log}");

    let refused = Host::new();
    let _session = runs(&refused, "project");
    refused.zellij.refuses_clients();
    let failed = on_terminal(luneta(refused.vars(), &directory("project").path));
    assert_eq!(failed.status.code(), Some(1), "{}", failed.said);
    let log = log_of(&refused);
    assert!(log.contains("ends: zellij's client failed: this zellij attaches no client"), "{log}");

    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "work", &json!([]));
    ssh.refuses_clients("far");
    hosts(&here, "far\n");
    let _daemon = start(here.vars()).unwrap();
    let failed = on_terminal(luneta(here.vars(), &directory("project").path));
    assert_eq!(failed.status.code(), Some(1), "{}", failed.said);
    let log = log_of(&here);
    assert!(log.contains("ends: no machine has a session to go on in"), "{log}");

    let told = Host::new();
    let _session = runs(&told, "project");
    told.zellij.listen();
    let ended = on_terminal_with(luneta(told.vars(), &directory("project").path), |luneta| {
        eventually("zellij's client started", || told.zellij.attached().first().cloned());
        stop(luneta.pid());
    });
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let log = log_of(&told);
    assert!(log.contains("ends: ended by signal 15"), "{log}");

    let unstarted = Host::new();
    let _session = runs(&unstarted, "project");
    let dir = directory("project");
    let mut alone = luneta(unstarted.vars(), &dir.path);
    let output = alone.env("ZELLIJ_SESSION_NAME", "project").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let log = log_of(&unstarted);
    assert!(log.contains("ends: this terminal is a pane of \"project\""), "{log}");
}

struct Machine {
    host: Host,
    _session: std::os::unix::net::UnixListener,
    _daemon: crate::host::Daemon,
}

fn machine(ssh: &FakeSsh, destination: &str, name: &str, panes: &serde_json::Value) -> Machine {
    let host = Host::new();
    host.zellij.list(name, panes);
    let session = host.serve(name);
    let daemon = start(host.vars()).unwrap();
    ssh.reaches(destination, host.vars());
    Machine { host, _session: session, _daemon: daemon }
}

#[test]
fn a_bare_luneta_with_no_session_here_joins_another_machine_s() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let far = machine(&ssh, "far", "work", &json!([]));
    let next = machine(&ssh, "next", "later", &json!([]));
    hosts(&here, "nowhere\nfar\nnext\n");
    let _daemon = start(here.vars()).unwrap();

    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("far"), ["zellij 'attach' 'work'"]);
    assert_eq!(ssh.clients("next"), [] as [String; 0]);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);
    assert!(far.host.zellij.attached().is_empty() && next.host.zellij.attached().is_empty());
}

#[test]
fn a_bare_luneta_goes_on_to_the_next_machine_where_a_client_fails() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "work", &json!([]));
    let _next = machine(&ssh, "next", "later", &json!([]));
    ssh.refuses_clients("far");
    hosts(&here, "far\nnext\n");
    let _daemon = start(here.vars()).unwrap();

    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("far"), ["zellij 'attach' 'work'"]);
    assert_eq!(ssh.clients("next"), ["zellij 'attach' 'later'"]);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);
}

#[test]
fn a_bare_luneta_passes_over_another_machine_s_session_zellij_does_not_attach_to() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "-c", &json!([]));
    let _next = machine(&ssh, "next", "later", &json!([]));
    hosts(&here, "far\nnext\n");
    let _daemon = start(here.vars()).unwrap();

    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("far"), [] as [String; 0]);
    assert_eq!(ssh.clients("next"), ["zellij 'attach' 'later'"]);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);
}

#[test]
fn a_bare_luneta_says_to_name_a_session_where_every_client_fails() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "work", &json!([]));
    ssh.refuses_clients("far");
    hosts(&here, "far\n");
    let _daemon = start(here.vars()).unwrap();

    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.said.contains("Connection refused"), "{}", ended.said);
    assert!(ended.said.contains("luneta <name>"), "{}", ended.said);
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);
}

#[test]
fn a_bare_luneta_whose_client_failed_never_goes_on_in_its_own_renamed_session() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "work", &json!([]));
    ssh.refuses_clients("far");
    hosts(&here, "far\n");
    let _renamed = here.serve("renamed");
    here.zellij.list("renamed", &pane_running("luneta"));
    let _daemon = start(here.vars()).unwrap();
    let dir = directory("project");
    let mut bare = luneta(here.vars(), &dir.path);
    bare.env("ZELLIJ_SESSION_NAME", "opened-as").env("ZELLIJ_PANE_ID", "1");

    let ended = on_terminal(bare);
    assert_eq!(ssh.clients("far"), ["zellij 'attach' 'work'"]);
    assert!(here.zellij.attached().is_empty(), "{:?}", here.zellij.attached());
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
}

#[test]
fn a_bare_luneta_makes_a_session_where_no_machine_has_one() {
    let here = Host::new();
    let _ssh = FakeSsh::new(here.zellij.bin());
    hosts(&here, "nowhere\n");
    let _daemon = start(here.vars()).unwrap();
    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(here.zellij.attached(), ["attach --create project"]);
}

#[test]
fn a_machine_that_says_nothing_is_waited_for_a_while_and_counts_for_none() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.waits_on("silent");
    hosts(&here, "silent\n");
    let _daemon = start(here.vars()).unwrap();

    let began = Instant::now();
    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert!(ended.said.contains("Waiting to hear what runs on silent"), "{}", ended.said);
    assert!(began.elapsed() < REACH, "it took {:?}", began.elapsed());
    assert_eq!(here.zellij.attached(), ["attach --create project"]);
}

#[test]
fn a_daemon_that_goes_away_does_not_cut_the_wait_short() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.waits_on("silent");
    hosts(&here, "silent\n");
    let daemon = start(here.vars()).unwrap();
    let goes_away = thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        drop(daemon);
    });

    let began = Instant::now();
    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    let took = began.elapsed();
    goes_away.join().unwrap();
    assert!(ended.status.success(), "{}", ended.said);
    assert!(took >= HEARING, "it took {took:?}");
    assert_eq!(here.zellij.attached(), ["attach --create project"]);
}

#[test]
fn a_daemon_that_goes_away_is_started_again_in_moments() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.waits_on("silent");
    hosts(&here, "silent\n");
    let daemon = start(here.vars()).unwrap();
    let was = daemon.pid();
    let dir = directory("project");
    thread::scope(|scope| {
        let waiting = scope.spawn(|| on_terminal(luneta(here.vars(), &dir.path)));
        thread::sleep(Duration::from_secs(3));
        let went = Instant::now();
        drop(daemon);
        let pid = eventually("a daemon answers again", || {
            answering(&here).filter(|answers| *answers != was)
        });
        let took = went.elapsed();
        let _daemon = Detached { host: &here, pid };
        assert!(took < Duration::from_secs(1), "it took {took:?}");
        let ended = waiting.join().unwrap();
        assert!(ended.status.success(), "{}", ended.said);
    });
}

#[test]
fn an_answer_of_another_daemon_than_the_one_reached_is_dropped() {
    const OTHER: u32 = 1;
    let here = Host::new();
    hosts(&here, "silent\n");
    let notes = here.home.path().join("notes");
    let stand_in = stand_in_watched_as(&here, OTHER, &notes);

    let began = Instant::now();
    let ended = on_terminal(luneta(here.vars(), &directory("project").path));
    let took = began.elapsed();
    assert!(ended.status.success(), "{}", ended.said);
    assert!(took >= HEARING, "it took {took:?}");

    let log = fs::read_to_string(here.home.path().join(".local/state/luneta/luneta.log")).unwrap();
    let note = format!(
        "lunetad (pid {OTHER}) answered in the place of the one luneta reached (pid {})",
        stand_in.pid()
    );
    assert!(log.contains(&note), "{log}");

    let notes = fs::read_to_string(&notes).unwrap();
    let asked = |what| notes.lines().filter(|line| *line == what).count();
    let (infos, watches) = (asked("info"), asked("watch"));
    assert!(infos >= 3, "it reached for a daemon {infos} times: {notes}");
    assert!(infos.abs_diff(watches) <= 1, "{infos} of Info to {watches} of Watch: {notes}");
}

#[test]
fn a_lunetad_that_does_not_start_is_started_once_a_second_at_the_most() {
    let here = Host::new();
    hosts(&here, "silent\n");
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let note = format!("echo started >> '{}'\nexit 1\n", started.display());
    script(&elsewhere.path().join("lunetad"), &note);
    let program = elsewhere.path().join("luneta").display().to_string();

    let began = Instant::now();
    let dir = directory("project");
    let ended = on_terminal(luneta_at(&program, here.vars(), &dir.path));
    let took = began.elapsed();
    assert!(ended.status.success(), "{}", ended.said);
    let starts = fs::read_to_string(&started).unwrap().lines().count();
    assert!(starts >= 2, "it was started {starts} times in {took:?}");
    let most = usize::try_from(took.as_secs()).unwrap() + 1;
    assert!(starts <= most, "it was started {starts} times in {took:?}");
}

fn pane_running(command: &str) -> serde_json::Value {
    let mut pane = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    pane["pane_command"] = json!(command);
    json!([pane])
}

#[test]
fn run_through_ssh_a_bare_luneta_passes_over_what_may_hold_it() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = machine(&ssh, "far", "held", &pane_running("ssh mac"));
    hosts(&here, "far\n");
    let _daemon = start(here.vars()).unwrap();
    let dir = directory("project");

    let mut held = luneta(here.vars(), &dir.path);
    held.env("SSH_TTY", "/dev/pts/9");
    let ended = on_terminal(held);
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    assert!(ended.said.contains("each session of far may hold this terminal"), "{}", ended.said);
    assert_eq!(ssh.clients("far"), [] as [String; 0]);
    assert_eq!(here.zellij.attached(), [] as [String; 0]);

    let _free = machine(&ssh, "free", "free", &pane_running("fish"));
    hosts(&here, "far\nfree\n");
    eventually("lunetad watches free", || {
        here.host().remotes.iter().any(|remote| remote.destination == "free").then_some(())
    });
    let mut free = luneta(here.vars(), &dir.path);
    free.env("SSH_TTY", "/dev/pts/9");
    let ended = on_terminal(free);
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("free"), ["zellij 'attach' 'free'"]);
}
