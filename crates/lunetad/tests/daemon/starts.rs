//! Where a bare `luneta` starts, as the client it runs says: on a terminal of
//! the test's, as it needs one, with a `zellij` and an `ssh` of the test's that
//! note the client they are asked for, and end, as one that detached does.

use std::fs::{self, File};
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::fs::{Mode, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};
use serde_json::json;
use tempfile::TempDir;

use crate::host::{eventually, start, Host, Running, Vars, LUNETA};
use crate::remote::{hosts, FakeSsh};
use crate::sessions::listed;

/// How long luneta has to end, in a test whose client ends at once: to give
/// up on its terminal's answers, to wait out what it waits for, and to end.
const ENDS: Duration = Duration::from_secs(30);

/// lunetad gives a machine that does not answer this long before it gives up.
const REACH: Duration = Duration::from_secs(10);

/// How long luneta waits to hear of a machine, from when it starts to.
const HEARING: Duration = Duration::from_secs(5);

/// How luneta ended, and what it wrote on its terminal.
struct Ended {
    status: ExitStatus,
    said: String,
}

/// Runs `command`, a luneta, on a terminal of its own until it ends. The
/// terminal is read all along, so that nothing waits on a write, and to its
/// end, once all that hold it have let go, as a `Command` does as it goes.
fn on_terminal(mut command: Command) -> Ended {
    let controller = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&controller, FdFlags::CLOEXEC).unwrap();
    pty::grantpt(&controller).unwrap();
    pty::unlockpt(&controller).unwrap();
    let path = pty::ptsname(&controller, Vec::new()).unwrap().into_string().unwrap();
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    let tty: OwnedFd = rustix::fs::open(&path, flags, Mode::empty()).unwrap();
    let size = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    termios::tcsetwinsize(&tty, size).unwrap();
    let reader = thread::spawn(move || {
        let mut said = Vec::new();
        // It ends in an error where it ends, with all there was to read in.
        let _ = File::from(controller).read_to_end(&mut said);
        said
    });
    let stdio = || Stdio::from(tty.try_clone().unwrap());
    let mut luneta = Running::spawn(command.stdin(stdio()).stdout(stdio()).stderr(stdio()));
    drop(command);
    drop(tty);
    let status = luneta.ended_within(ENDS).expect("luneta did not end");
    let said = reader.join().unwrap();
    Ended { status, said: String::from_utf8_lossy(&said).into_owned() }
}

/// A folder of the test's, named `name`, for luneta to start in.
struct Directory {
    _parent: TempDir,
    path: PathBuf,
}

fn directory(name: &str) -> Directory {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join(name);
    fs::create_dir(&path).unwrap();
    Directory { _parent: parent, path }
}

/// A luneta of `vars`, started in `dir`, in no ssh session.
fn luneta(vars: Vars, dir: &Path) -> Command {
    let mut command = vars.command(LUNETA);
    command.current_dir(dir).env_remove("SSH_CONNECTION").env_remove("SSH_TTY");
    command
}

/// Has `name` run on `host`, as zellij answers for it: whatever the socket
/// that this returns, which it listens on while it is kept.
fn runs(host: &Host, name: &str) -> std::os::unix::net::UnixListener {
    host.zellij.list(name, &json!([]));
    host.serve(name)
}

/// A bare luneta joins what runs, by `attach` alone: the session named for
/// its directory, though another is newer, or else the newest. `--create`
/// would make a session gone since luneta looked again, empty.
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

/// Of the sessions you were in, as the news keeps it for every luneta, a bare
/// luneta joins the last, though another is newer: the news goes by the socket
/// of each as the daemon lists it.
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

/// With none running, a bare luneta makes the session of its directory: a
/// socket whose server is gone is none, and neither is a session that ended,
/// which zellij brings back by that name.
#[test]
fn a_bare_luneta_makes_a_session_only_where_none_runs() {
    let host = Host::new();
    host.keep("ended");
    let _gone = host.serve("gone");
    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach --create project"]);
}

/// A session named is asked for: it is attached to, made if none runs, though
/// another runs. This zellij makes none, so that luneta goes on in the other
/// after, as the session it took for its client's has gone with it.
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

/// Run in a pane of a session, a bare luneta joins another, though it is the
/// one named for its directory that it is a pane of; and where no other is
/// left, it makes none, and says so before it touches its terminal.
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
    assert!(host.zellij.attached().is_empty());

    let _other = runs(&host, "other");
    let mut other = luneta(host.vars(), &dir.path);
    other.env("ZELLIJ_SESSION_NAME", "project");
    let ended = on_terminal(other);
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach other"]);
}

/// A rename leaves the name a pane opened under behind in what runs there. So
/// a bare luneta goes by what zellij lists now as well: it passes over a
/// session whose pane of its id runs luneta, or what zellij has yet to say, and
/// makes none. It joins one whose pane of that id runs something else.
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
    assert!(host.zellij.attached().is_empty());

    let _other = host.serve("other");
    host.zellij.list("other", &pane_running("fish"));
    let ended = on_terminal(in_a_pane());
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach other"]);
}

/// A hosts file that does not read is said to be so, as it leaves luneta
/// looking on no other machine, which is not to say that none is named.
#[test]
fn a_bare_luneta_says_where_the_hosts_file_does_not_read() {
    let host = Host::new();
    fs::create_dir_all(host.home.path().join(".local/state/luneta/hosts")).unwrap();
    let ended = on_terminal(luneta(host.vars(), &directory("project").path));
    assert!(ended.status.success(), "{}", ended.said);
    assert!(ended.said.contains("luneta/hosts: "), "{}", ended.said);
    assert_eq!(host.zellij.attached(), ["attach --create project"]);
}

/// A machine of the hosts file, reached by `ssh`, with `name` running there.
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

/// With no session here, a bare luneta joins another machine's, of the first
/// that has one by the hosts file: one lunetad does not reach has none, and
/// nothing is made here.
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
    assert_eq!(ssh.clients("far"), ["zellij attach 'work'"]);
    assert!(ssh.clients("next").is_empty());
    assert!(here.zellij.attached().is_empty());
    assert!(far.host.zellij.attached().is_empty() && next.host.zellij.attached().is_empty());
}

/// A client of another machine's session that fails has luneta go on to the
/// next machine's, as it goes on once a session ended.
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
    assert_eq!(ssh.clients("far"), ["zellij attach 'work'"]);
    assert_eq!(ssh.clients("next"), ["zellij attach 'later'"]);
    assert!(here.zellij.attached().is_empty());
}

/// With no session anywhere, a bare luneta makes its directory's, though a
/// machine is named that lunetad does not reach.
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

/// luneta waits for a machine that says nothing no longer than it can bear,
/// less than lunetad gives it, and says what it waits for meanwhile. It counts
/// as having none from then on.
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

/// A daemon that goes away as luneta waits is looked for again, as one can end
/// as it is reached, or make way for a newer: a machine still counts as having
/// none only once the wait has run its course.
#[test]
fn a_daemon_that_goes_away_does_not_cut_the_wait_short() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.waits_on("silent");
    hosts(&here, "silent\n");
    let daemon = start(here.vars()).unwrap();
    // Well into the wait, which starts once luneta gave up on its terminal's
    // answers, about a second in.
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

/// The one pane of a session, as zellij lists it, that runs `command`.
fn pane_running(command: &str) -> serde_json::Value {
    let mut pane = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    pane["pane_command"] = json!(command);
    json!([pane])
}

/// Run through ssh, a bare luneta passes over a machine's session that may
/// hold its terminal, as a pane of it runs ssh: it hears of the panes first,
/// and joins one that does not. Where only those are left, it makes none, and
/// says so.
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
    assert!(ssh.clients("far").is_empty());
    assert!(here.zellij.attached().is_empty());

    let _free = machine(&ssh, "free", "free", &pane_running("fish"));
    hosts(&here, "far\nfree\n");
    eventually("lunetad watches free", || {
        here.host().remotes.iter().any(|remote| remote.destination == "free").then_some(())
    });
    let mut free = luneta(here.vars(), &dir.path);
    free.env("SSH_TTY", "/dev/pts/9");
    let ended = on_terminal(free);
    assert!(ended.status.success(), "{}", ended.said);
    assert_eq!(ssh.clients("free"), ["zellij attach 'free'"]);
}
