//! The other machines a daemon watches, as its hosts file names them, through
//! `ssh <destination> luneta bridge`: here, through an ssh of the test's own,
//! which runs the bridge of another host of the test's.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::remote::unreached::{self, Stopped};
use lunetad::api::remote::{self, Reached};
use lunetad::api::refuge_response::{Ended, Kept, Went};
use lunetad::api::{self, Client, GoRequest, ListClientsRequest, On, RefugeRequest, WatchRequest};
use tonic::Code;

use crate::demand::Detached;
use crate::host::{eventually, folder_in, script, start, Host, Vars, LUNETA};

/// An `ssh` of a test's own, in a folder to put first on the `PATH` of a
/// host's daemon, that reaches the test's other hosts by the destinations it
/// was given for them with `reaches`: it runs `luneta bridge` with their
/// variables, as ssh runs it on the machine it reaches. Of a destination it
/// was given none for, it says it cannot resolve it, and ends as ssh does
/// then, with 255. Nor does it run anything on a terminal, which would mangle
/// what the daemons say, or where it could ask for a password, which no one
/// would answer: without `-T` and `BatchMode=yes`, it ends so too. It notes
/// each time it is run to a destination, for `runs`, and the pid of each
/// bridge it runs, for `bridges`.
struct FakeSsh {
    to: PathBuf,
}

impl FakeSsh {
    /// Writes the `ssh` in `bin`.
    fn new(bin: &Path) -> Self {
        let ssh = FakeSsh { to: bin.join("ssh-to") };
        fs::create_dir(&ssh.to).unwrap();
        let text = format!(
            r#"quiet= batch=
while [ $# -gt 0 ]; do
    case "$1" in
        -T) quiet=yes; shift ;;
        -o) [ "$2" = BatchMode=yes ] && batch=yes; shift 2 ;;
        -*) echo "this ssh knows no $1" >&2; exit 255 ;;
        *) break ;;
    esac
done
if [ -z "$quiet" ] || [ -z "$batch" ]; then
    echo "this ssh runs nothing on a terminal, or where it could ask" >&2
    exit 255
fi
destination="$1"
shift
echo run >> "{to}/$destination.runs"
if [ ! -x "{to}/$destination" ]; then
    echo "ssh: Could not resolve hostname $destination: Name or service not known" >&2
    exit 255
fi
if [ "$*" != "luneta bridge" ]; then
    echo "this ssh runs luneta bridge alone, and was asked: $*" >&2
    exit 255
fi
echo $$ >> "{to}/$destination.bridges"
exec "{to}/$destination"
"#,
            to = ssh.to.display()
        );
        script(&bin.join("ssh"), &text);
        ssh
    }

    /// Has ssh reach the host whose variables are `vars` as `destination`.
    fn reaches(&self, destination: &str, vars: Vars) {
        let bridge = vars.command(LUNETA);
        let (mut unset, mut set) = (String::new(), String::new());
        for (name, value) in bridge.get_envs() {
            let name = name.to_str().unwrap();
            match value {
                None => write!(unset, " -u {name}").unwrap(),
                Some(value) => write!(set, " {name}={}", quoted(value.to_str().unwrap())).unwrap(),
            }
        }
        let text = format!("exec env{unset}{set} {} bridge\n", quoted(LUNETA));
        script(&self.to.join(destination), &text);
    }

    /// Has ssh reach `destination` no more, as one that cannot resolve it.
    fn unreaches(&self, destination: &str) {
        fs::remove_file(self.to.join(destination)).unwrap();
    }

    /// How many times ssh has been run to `destination`.
    fn runs(&self, destination: &str) -> usize {
        let runs = fs::read_to_string(self.to.join(format!("{destination}.runs")));
        runs.unwrap_or_default().lines().count()
    }

    /// The pids of the bridges ssh has run to `destination`, in turn.
    fn bridges(&self, destination: &str) -> Vec<u32> {
        let path = self.to.join(format!("{destination}.bridges"));
        let bridges = fs::read_to_string(path).unwrap_or_default();
        bridges.lines().map(|pid| pid.parse().unwrap()).collect()
    }
}

/// `text` as the shell reads it as one word, whatever is in it.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Has `host`'s daemon watch the machines `text` names, as its hosts file,
/// written whole before the daemon can read it.
fn hosts(host: &Host, text: &str) {
    let dir = host.home.path().join(".local/state/luneta");
    fs::create_dir_all(&dir).unwrap();
    let writing = dir.join("hosts.writing");
    fs::write(&writing, text).unwrap();
    fs::rename(writing, dir.join("hosts")).unwrap();
}

/// The host of the machine `host`'s daemon watches as `destination`, once
/// it has reached it and `wanted` takes what it says.
fn reached(
    host: &Host,
    destination: &str,
    wanted: impl Fn(&remote::Host) -> bool,
) -> remote::Host {
    eventually(&format!("{destination} is reached as wanted"), || {
        let remote = host.host().remotes.into_iter().find(|it| it.destination == destination)?;
        match remote.reached? {
            Reached::Host(reached) if wanted(&reached) => Some(reached),
            _ => None,
        }
    })
}

/// What `host`'s daemon has of the machine it watches as `destination`, once
/// it is unreached.
fn unreached(host: &Host, destination: &str) -> remote::Unreached {
    eventually(&format!("{destination} is unreached"), || {
        let remote = host.host().remotes.into_iter().find(|it| it.destination == destination)?;
        match remote.reached? {
            Reached::Unreached(unreached) => Some(unreached),
            _ => None,
        }
    })
}

/// The names of the sessions of the machine `host`, in order.
fn names(host: &remote::Host) -> Vec<&str> {
    let mut names: Vec<&str> = host.sessions.iter().map(|it| it.name.as_str()).collect();
    names.sort_unstable();
    names
}

/// Whether the process `pid` has ended, whether or not its parent has
/// waited for it yet.
fn ended(pid: u32) -> bool {
    let stat = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8(stat.stdout).unwrap();
    stat.trim().is_empty() || stat.trim().starts_with('Z')
}

/// A daemon watches each machine its hosts file names, in the file's order,
/// through ssh: one whose daemon answers, with that daemon's host, as it
/// changes, and one ssh cannot reach, with why, and that ssh ended so. A
/// line ssh would take for an option names none, and the daemon says that
/// it could not read it. `luneta sessions` lists the sessions of each
/// machine under it, and says so too.
#[test]
fn a_daemon_says_each_machine_its_hosts_file_names() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "nowhere\n-oProxyCommand=sh\nfar\n");
    let _here = start(here.vars()).unwrap();

    let first = reached(&here, "far", |far| !far.sessions.is_empty());
    assert_eq!(names(&first), ["work"]);
    let _play = far.serve("play");
    reached(&here, "far", |far| names(far) == ["play", "work"]);

    let host = here.host();
    assert_eq!(host.unread_hosts, Some(1));
    let remotes = host.remotes;
    let order: Vec<&str> = remotes.iter().map(|it| it.destination.as_str()).collect();
    assert_eq!(order, ["nowhere", "far"]);
    match &remotes[0].reached {
        Some(Reached::Unreached(unreached)) => {
            let ended = Stopped::Ended(unreached::Ended { code: Some(255) });
            assert_eq!(unreached.stopped, Some(ended), "{}", unreached.why);
        },
        other => panic!("nowhere is {other:?}"),
    }
    let output = here.vars().command(LUNETA).arg("sessions").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let said = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert!(lines[0].starts_with("nowhere/  (unreached for "), "{said}");
    let far = ["far/", "  play  (no panes listed yet)", "  work  (no panes listed yet)"];
    assert_eq!(lines[1..4], far, "{said}");
    let unread = "(lunetad could not read all of luneta's hosts file: its log says why)";
    assert_eq!(lines[4..], [unread], "{said}");
}

/// A hosts file that does not read leaves the machines it named as they
/// were, and the daemon says that it could not read it, until it reads
/// again.
#[test]
fn a_hosts_file_that_does_not_read_leaves_the_machines_as_they_were() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    assert_eq!(here.host().unread_hosts, Some(0));

    // A folder in its place, which no one can read as a file.
    let file = here.home.path().join(".local/state/luneta/hosts");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    let host = eventually("the file does not read", || {
        let host = here.host();
        (host.unread_hosts == Some(1)).then_some(host)
    });
    let machines: Vec<&str> = host.remotes.iter().map(|it| it.destination.as_str()).collect();
    assert_eq!(machines, ["far"]);

    fs::remove_dir(&file).unwrap();
    hosts(&here, "far\n");
    eventually("the file reads", || (here.host().unread_hosts == Some(0)).then_some(()));
}

/// A machine that leaves the hosts file leaves the daemon's host, and the
/// bridge to it ends.
#[test]
fn a_machine_that_leaves_the_hosts_file_is_watched_no_more() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let bridges = ssh.bridges("far");
    assert_eq!(bridges.len(), 1);

    hosts(&here, "");
    eventually("far leaves", || here.host().remotes.is_empty().then_some(()));
    eventually("the bridge to far ends", || ended(bridges[0]).then_some(()));
}

/// The daemon of a machine that ends, as one that makes way for a newer one
/// does, ends the bridge to it, and the daemon that watches the machine runs
/// ssh again, which starts another there.
#[test]
fn a_machine_whose_daemon_ends_is_reached_again() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    drop(far_daemon);
    let _later = far.serve("later");
    reached(&here, "far", |far| names(far) == ["later"]);
    let _again = Detached { host: &far, pid: far.host().version.unwrap().pid };
}

/// A machine whose daemon ends, where ssh no longer reaches, is unreached
/// since its daemon last answered, as ssh ended that tried it again.
#[test]
fn a_machine_ssh_no_longer_reaches_is_unreached_since_it_last_answered() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    ssh.unreaches("far");
    let gone = SystemTime::now();
    drop(far_daemon);
    let unreached = unreached(&here, "far");
    let ssh_ended = Stopped::Ended(unreached::Ended { code: Some(255) });
    assert_eq!(unreached.stopped, Some(ssh_ended), "{}", unreached.why);
    let since = SystemTime::try_from(unreached.since.unwrap()).unwrap();
    assert!(since <= gone, "unreached since {since:?}, after its daemon ended");
}

/// Two daemons that watch each other come to rest: each says the other's
/// host as a machine's, which has neither the other's remotes nor its
/// version, which would else change each time the other's does, and the
/// other's each time it does, for as long as both ran.
#[test]
fn two_daemons_that_watch_each_other_come_to_rest() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    FakeSsh::new(far.zellij.bin()).reaches("here", here.vars());
    hosts(&here, "far\n");
    hosts(&far, "here\n");
    let _here = start(here.vars()).unwrap();
    let _far = start(far.vars()).unwrap();
    reached(&here, "far", |_| true);
    reached(&far, "here", |_| true);

    // The daemon answers with the host as it was once it has not changed
    // for 4 seconds.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = crate::host::folder_in(here.runtime.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let mut after = None;
        loop {
            let asked = WatchRequest { after, kept: None };
            let watched = daemon.watch(asked).await.unwrap().into_inner();
            if watched.version.is_some() && watched.version == after {
                return;
            }
            assert!(Instant::now() < deadline, "the host never came to rest");
            after = watched.version;
        }
    });
}

/// Watches `host`'s daemon until `until`, as a luneta does: asks again as
/// soon as it answers.
fn watch_until(host: &Host, until: Instant) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = crate::host::folder_in(host.runtime.path()).unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let mut after = None;
        while Instant::now() < until {
            let asked = daemon.watch(WatchRequest { after, kept: None }).await;
            after = asked.expect("the daemon ended").into_inner().version;
        }
    });
}

/// A daemon that a client keeps keeps the daemons of the machines it watches
/// running, however long zellij there has had no session, as a client there
/// would: `luneta bridge` would else start one again each time one ended.
/// The test watches the one for longer than the other would run on alone.
#[test]
fn a_daemon_a_client_keeps_keeps_the_machines_it_watches_running() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let mut far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    watch_until(&here, Instant::now() + Duration::from_secs(36));
    assert_eq!(far_daemon.ended_within(Duration::ZERO), None);
    assert_eq!(ssh.bridges("far").len(), 1);
}

/// Two daemons that watch each other, which nothing else keeps, end as each
/// would alone: each says to the other that it runs for no reason of its
/// own. Nor does either run ssh again as the other ends, which would start
/// the other's again through its `luneta bridge`, and so on for good. The
/// test waits it out.
#[test]
fn two_daemons_that_watch_each_other_and_nothing_else_end() {
    let here = Host::new();
    let far = Host::new();
    let to_far = FakeSsh::new(here.zellij.bin());
    to_far.reaches("far", far.vars());
    let to_here = FakeSsh::new(far.zellij.bin());
    to_here.reaches("here", here.vars());
    hosts(&here, "far\n");
    hosts(&far, "here\n");
    let mut here_daemon = start(here.vars()).unwrap();
    let mut far_daemon = start(far.vars()).unwrap();
    reached(&here, "far", |_| true);
    reached(&far, "here", |_| true);

    let ended = here_daemon.ended_within(Duration::from_secs(55));
    assert!(ended.is_some(), "{:?}", here_daemon.said_after(Duration::ZERO));
    let ended = far_daemon.ended_within(Duration::from_secs(15));
    assert!(ended.is_some(), "{:?}", far_daemon.said_after(Duration::ZERO));
    assert_eq!(to_far.bridges("far").len(), 1);
    assert_eq!(to_here.bridges("here").len(), 1);
}

/// What ssh says of a machine it does not reach goes in the log once, while
/// each ssh to it says it again: one that stays unreached is run again and
/// again, for as long as the daemon runs.
#[test]
fn what_each_ssh_to_a_machine_says_again_is_said_once() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    hosts(&here, "nowhere\n");
    let mut daemon = start(here.vars()).unwrap();
    // Watched, so that it is kept, and runs ssh.
    eventually("ssh runs to nowhere a few times", || {
        here.host();
        (ssh.runs("nowhere") >= 3).then_some(())
    });

    let said = daemon.said_after(Duration::from_secs(1));
    let unresolved = "nowhere: ssh: Could not resolve hostname nowhere";
    let lines = said.iter().filter(|line| line.contains(unresolved)).count();
    assert_eq!(lines, 1, "{said:?}");
}

/// `luneta ssh` adds a machine to the hosts file, after the machines it
/// names, and says that lunetad reaches it once one runs; and where one
/// runs, how its first ssh there went: reached, or not, and why, and it
/// fails then. A machine the file names already stays named once.
#[test]
fn luneta_ssh_adds_a_machine_and_says_how_lunetad_reaches_it() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "nowhere\n");
    let ssh_to = |destination: &str| {
        let output = here.vars().command(LUNETA).args(["ssh", destination]).output().unwrap();
        let said = String::from_utf8(output.stdout).unwrap();
        (output.status.success(), said, String::from_utf8(output.stderr).unwrap())
    };

    let (ok, said, _) = ssh_to("far");
    assert!(ok);
    assert!(said.starts_with("far: added to luneta's hosts file\nfar: lunetad reaches it once"));
    let file = here.home.path().join(".local/state/luneta/hosts");
    assert_eq!(fs::read_to_string(&file).unwrap(), "nowhere\nfar\n");

    let _here = start(here.vars()).unwrap();
    let (ok, said, _) = ssh_to("far");
    let lines: Vec<&str> = said.lines().collect();
    assert!(ok, "{said}");
    assert_eq!(lines[0], "far: luneta's hosts file names it already");
    assert!(lines[1].starts_with("far: reached, "), "{said}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "nowhere\nfar\n");

    let (ok, said, stderr) = ssh_to("elsewhere");
    assert!(!ok);
    assert_eq!(said, "elsewhere: added to luneta's hosts file\n");
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(lines[0].starts_with("luneta: elsewhere: unreached: "), "{stderr}");
    let log = here.home.path().join(".local/state/luneta/luneta.log");
    assert_eq!(lines[1], format!("luneta: what ssh said of it is in {}", log.display()));
    // What ssh said is heard on a thread of its own, as the daemon tells of
    // its end.
    let said = "elsewhere: ssh: Could not resolve hostname elsewhere";
    eventually("what ssh said is in the log", || {
        fs::read_to_string(&log).ok().filter(|logged| logged.contains(said))
    });
}

/// A daemon hands what a client asks of zellij on another machine to that
/// machine's daemon, through the bridge it watches it on, which asks zellij
/// there: where to take a client, a session's clients, and where a client
/// goes on once its session ended. Without a machine, it asks its own; of
/// one it does not reach, it says so.
#[test]
fn a_daemon_hands_on_what_is_asked_of_another_machine_s_zellij() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _work = far.serve("work");
    far.zellij.list("work", &serde_json::json!([]));
    far.zellij.clients("work", &[(1, "terminal_3")]);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let (at, to) = ("work".to_owned(), "play".to_owned());
        let go = GoRequest { on: on("far"), at, to, pane: Some(2) };
        daemon.go(go).await.unwrap();
        let went = ["--session work action switch-session play --pane-id terminal_2"];
        assert_eq!(far.zellij.went(), went);
        assert!(here.zellij.went().is_empty());

        let work = |on| ListClientsRequest { on, session: "work".to_owned() };
        let listed = daemon.list_clients(work(on("far"))).await.unwrap().into_inner();
        assert_eq!(listed.clients, [Client { id: 1, pane: Some(3) }]);
        let here_none = daemon.list_clients(work(None)).await.unwrap_err();
        assert_eq!(here_none.code(), Code::FailedPrecondition, "{here_none:?}");
        let nowhere = daemon.list_clients(work(on("nowhere"))).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::Unavailable, "{nowhere:?}");

        let seats = |seat: &str| vec![seat.to_owned()];
        let ended = RefugeRequest { on: on("far"), seats: seats("gone") };
        let found = daemon.refuge(ended).await.unwrap().into_inner();
        let refuge = Some(api::Refuge { session: "work".to_owned(), before: listed.clients });
        assert_eq!(found.ended, Some(Ended::Went(Went { refuge })));
        let detached = RefugeRequest { on: on("far"), seats: seats("work") };
        let found = daemon.refuge(detached).await.unwrap().into_inner();
        assert_eq!(found.ended, Some(Ended::Kept(Kept {})));

        // A question that names no session, or none the client was in, asks
        // nothing of zellij, here or there.
        let (at, to) = (String::new(), "play".to_owned());
        let nameless = daemon.go(GoRequest { on: on("far"), at, to, pane: None }).await.unwrap_err();
        assert_eq!(nameless.code(), Code::InvalidArgument, "{nameless:?}");
        let seatless = RefugeRequest { on: on("far"), seats: Vec::new() };
        let seatless = daemon.refuge(seatless).await.unwrap_err();
        assert_eq!(seatless.code(), Code::InvalidArgument, "{seatless:?}");
        assert_eq!(far.zellij.went(), went);
    });
}
