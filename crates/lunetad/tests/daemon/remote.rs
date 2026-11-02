//! The other machines a daemon watches, as its hosts file names them, through
//! `ssh <destination> luneta bridge`: here, through an ssh of the test's own,
//! which runs the bridge of another host of the test's.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::remote::unreached::{self, Stopped};
use lunetad::api::remote::{self, Reached};
use lunetad::api::keep_printed_response::{self, File};
use lunetad::api::{close_request, closes_response};
use lunetad::api::list_skills_response::Skill;
use lunetad::api::open_request::{self, Opens};
use lunetad::api::refuge_response::{Ended, Kept, Went};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::{
    self, AskRequest, Client, CloseRequest, ClosesRequest, GoRequest, KeepPrintedRequest,
    KillRequest, ListClientsRequest, ListContentsRequest, ListDirsRequest, ListSkillsRequest,
    NameSessionRequest, On, OpenRequest, RefugeRequest, StartRequest, TranscriptEndsRequest,
    WatchRequest,
};
use lunetad::asks::{Asked, Step, Turn, User};
use lunetad::links::{self, At, Link, Pasted, Ready};
use lunetad::transcripts::SessionId;
use lunetad::SessionName;
use rustix::process::{test_kill_process, Pid};
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::demand::Detached;
use crate::host::{eventually, folder_in, script, start, Host, Vars, LUNETA};
use crate::worktree::{cloned_as_wt, git};

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

/// A daemon hands what a client asks of another machine to that machine's
/// daemon, through the bridge it watches it on, which asks zellij there, or
/// looks there: where to take a client, a session's clients, where a client
/// goes on once its session ended, a pane or a tab to open, the skills of a
/// directory, what a pane's shell printed, which it keeps there, and the
/// name a session for a directory takes there, and to start it, and what
/// closing a pane closes there, to close it, and a session to kill, and the
/// ends of transcripts in Claude Code's folder there. Without a machine, it
/// asks its own; of one it does not reach, it says so. A question that names
/// a directory by no absolute path, an empty line typed, a tab by an empty
/// name, a tab to close of no terminal, or a transcript by no session's id,
/// or more of the transcripts than a brief keeps, or none of them, is
/// invalid here; no tab opens there in a session no client is in, and
/// nothing closes there that zellij lists otherwise than the close says.
#[test]
fn a_daemon_hands_on_what_is_asked_of_another_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    // A claude there that lists no plugins, whatever this machine has.
    script(&far.zellij.bin().join("claude"), "echo '[]'\n");
    let _work = far.serve("work");
    let tab = serde_json::json!({
        "id": 4, "is_plugin": false, "is_selectable": true, "is_suppressed": false,
        "is_floating": false, "tab_id": 3,
    });
    far.zellij.list("work", &serde_json::json!([tab]));
    far.zellij.clients("work", &[(1, "terminal_3")]);
    far.zellij.list("idle", &serde_json::json!([tab]));
    let beside = serde_json::json!({
        "id": 5, "is_plugin": false, "is_selectable": true, "is_suppressed": false,
        "is_floating": false, "tab_id": 6,
    });
    far.zellij.list("two", &serde_json::json!([tab, beside]));
    let holds = serde_json::json!({
        "id": 8, "is_plugin": false, "is_selectable": true, "is_suppressed": false,
        "is_floating": false, "tab_id": 3,
    });
    far.zellij.list("held", &serde_json::json!([tab, holds, beside]));
    far.zellij.dumps("work", "~> cargo test\nerror[E0308]\n~>\n");
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join(".claude/skills/near");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "---\ndescription: Near.\n---\n").unwrap();
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    let listed = reached(&here, "far", |far| !far.sessions.is_empty());
    let work_started = listed.sessions.iter().find(|it| it.name == "work").unwrap().started_at;

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

        let open = |opens| OpenRequest {
            on: on("far"),
            session: "work".to_owned(),
            cwd: "/w".to_owned(),
            command: vec!["claude".to_owned()],
            opens,
        };
        let pane = daemon.open(open(Some(Opens::Beside(2)))).await.unwrap().into_inner();
        let tab = Opens::Tab(open_request::Tab { name: Some("#5".to_owned()), alone_since: None });
        let tab = daemon.open(open(Some(tab))).await.unwrap().into_inner();
        assert_eq!((pane.terminal, tab.terminal), (9, 4));
        let opened = [
            "2 --session work action new-pane --no-focus --cwd /w -- claude",
            "- --session work action new-tab --no-focus --cwd /w --name #5 -- claude",
        ];
        assert_eq!(far.zellij.opened(), opened);
        let nothing = daemon.open(open(None)).await.unwrap_err();
        assert_eq!(nothing.code(), Code::InvalidArgument, "{nothing:?}");
        let invalid =
            |status: Status| assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        let relative = OpenRequest { cwd: "w".to_owned(), ..open(Some(Opens::Beside(2))) };
        invalid(daemon.open(relative).await.unwrap_err());
        let unnamed = |name: &str| {
            Opens::Tab(open_request::Tab { name: Some(name.to_owned()), alone_since: None })
        };
        invalid(daemon.open(open(Some(unnamed("")))).await.unwrap_err());
        let idle = OpenRequest { session: "idle".to_owned(), ..open(Some(unnamed("#5"))) };
        let idle = daemon.open(idle).await.unwrap_err();
        assert_eq!(idle.code(), Code::FailedPrecondition, "{idle:?}");
        assert!(idle.message().contains("no client is in idle"), "{idle:?}");
        assert_eq!(far.zellij.opened(), opened);
        assert!(here.zellij.opened().is_empty());

        let skills = dir.path().to_str().unwrap().to_owned();
        let skills = ListSkillsRequest { on: on("far"), dir: skills };
        let found = daemon.list_skills(skills).await.unwrap();
        let near = Skill { name: "near".to_owned(), description: "Near.".to_owned() };
        assert_eq!(found.into_inner().skills, [near]);
        let nowhere = ListSkillsRequest { on: on("far"), dir: String::new() };
        invalid(daemon.list_skills(nowhere).await.unwrap_err());

        let typed = "cargo test".to_owned();
        let session = "work".to_owned();
        let printed = KeepPrintedRequest { on: on("far"), session, pane: 2, typed };
        let kept = daemon.keep_printed(printed).await.unwrap().into_inner().printed;
        let Some(keep_printed_response::Printed::After(File { path, lines: 1, left_out: 0 })) = kept
        else {
            panic!("kept {kept:?}");
        };
        assert!(Path::new(&path).starts_with(far.runtime.path().canonicalize().unwrap()), "{path}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "error[E0308]\n");
        let (session, typed) = ("work".to_owned(), String::new());
        let untyped = KeepPrintedRequest { on: on("far"), session, pane: 2, typed };
        invalid(daemon.keep_printed(untyped).await.unwrap_err());

        // A directory named for a session that runs there takes the name after.
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        let work = work.to_str().unwrap().to_owned();
        let before = SystemTime::now();
        let named = NameSessionRequest { on: on("far"), dir: work.clone() };
        let named = daemon.name_session(named).await.unwrap().into_inner();
        assert_eq!(named.name, "work-2");
        // Just before the daemon there read the names, by its clock.
        let named_at = named.named_at.map(|at| SystemTime::try_from(at).unwrap());
        assert!(named_at.is_some_and(|at| at >= before && at <= SystemTime::now()), "{named_at:?}");
        let relative = NameSessionRequest { on: on("far"), dir: "work".to_owned() };
        invalid(daemon.name_session(relative).await.unwrap_err());
        let (at, to, cwd) = ("work".to_owned(), named.name.clone(), Some("work".to_owned()));
        invalid(daemon.start(StartRequest { on: on("far"), at, to, cwd }).await.unwrap_err());
        let (at, cwd) = ("work".to_owned(), Some(work.clone()));
        daemon.start(StartRequest { on: on("far"), at, to: named.name, cwd }).await.unwrap();
        let started = format!("--session work action switch-session work-2 --cwd {work}");
        assert_eq!(far.zellij.went().last(), Some(&started));
        assert!(here.zellij.went().is_empty());

        // Closing a pane there closes what zellij there lists it closes: the
        // tab of one in a tab beside another, the session of the only tab's.
        let closes = |session: &str| {
            let session = session.to_owned();
            ClosesRequest { on: on("far"), session, pane: 4 }
        };
        let two = daemon.closes(closes("two")).await.unwrap().into_inner().closes;
        let tab = closes_response::Tab { id: 3, panes: vec![4] };
        assert_eq!(two, Some(closes_response::Closes::Tab(tab)));
        let idle = daemon.closes(closes("idle")).await.unwrap().into_inner().closes;
        let whole = closes_response::Closes::Session(closes_response::Session {});
        assert_eq!(idle, Some(whole));
        // What it closes goes while zellij lists it as the close says: a pane
        // another holds its tab open beside, and a tab with its terminals.
        let close = |session: &str, closes| {
            CloseRequest { on: on("far"), session: session.to_owned(), closes: Some(closes) }
        };
        let pane = close_request::Closes::Pane;
        let tab = |id, panes| close_request::Closes::Tab(closes_response::Tab { id, panes });
        daemon.close(close("held", pane(4))).await.unwrap();
        daemon.close(close("two", tab(3, vec![4]))).await.unwrap();
        for (session, closes, why) in [
            ("two", pane(4), "pane 4 would take its tab along now"),
            ("two", tab(6, vec![5, 9]), "tab 6 holds other panes than it did"),
            ("idle", tab(3, vec![4]), "tab 3 would take the session along now"),
            ("two", pane(9), "pane 9 is gone"),
        ] {
            let stays = daemon.close(close(session, closes)).await.unwrap_err();
            assert_eq!(stays.code(), Code::FailedPrecondition, "{stays:?}");
            assert!(stays.message().contains(why), "{stays:?}");
        }
        invalid(daemon.close(close("two", tab(3, Vec::new()))).await.unwrap_err());
        let none = CloseRequest { closes: None, ..close("two", pane(4)) };
        invalid(daemon.close(none).await.unwrap_err());
        let closed_there = [
            "--session held action close-pane --pane-id terminal_4",
            "--session two action close-tab-by-id 3",
        ];
        assert_eq!(far.zellij.closed(), closed_there);

        // A session there is killed by when it started, as its daemon lists
        // it, and deleted.
        let kill = |started_at| {
            KillRequest { on: on("far"), session: "work".to_owned(), started_at }
        };
        invalid(daemon.kill(kill(None)).await.unwrap_err());
        daemon.kill(kill(work_started)).await.unwrap();
        assert_eq!(far.zellij.closed()[2..], ["delete-session --force work"]);
        assert!(here.zellij.closed().is_empty());

        // A transcript there is read there, and one not written yet has no
        // end.
        let (id, unwritten) =
            ("0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60", "9e8d7c6b-5a4f-4e3d-2c1b-0a9f8e7d6c5b");
        let project = far.home.path().join(".claude/projects/-w");
        fs::create_dir_all(&project).unwrap();
        let said = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        fs::write(project.join(format!("{id}.jsonl")), format!("{said}\n")).unwrap();
        let ends = |on, ids: &[&str], share| {
            let ids = ids.iter().map(|id| (*id).to_owned()).collect();
            TranscriptEndsRequest { on, ids, share }
        };
        let read = daemon.transcript_ends(ends(on("far"), &[id, unwritten], 1024)).await.unwrap();
        assert_eq!(read.into_inner().ends, HashMap::from([(id.to_owned(), said.to_owned())]));
        let read = daemon.transcript_ends(ends(None, &[id], 1024)).await.unwrap();
        assert!(read.into_inner().ends.is_empty());
        invalid(daemon.transcript_ends(ends(on("far"), &["../x"], 1024)).await.unwrap_err());
        let long = ends(on("far"), &[id, unwritten], 512 * 1024);
        invalid(daemon.transcript_ends(long).await.unwrap_err());
        invalid(daemon.transcript_ends(ends(on("far"), &[id], 0)).await.unwrap_err());
    });
}

/// A `claude` for `host` that takes turns of the ask box: it writes where it
/// ran and what it was told in `host`'s home, `told` and `words` there, and
/// says it starts a session, and runs `luneta here`, and says where the user
/// is. On words that say `please-fail`, it fails as one with no login does;
/// on `please-be-slow`, it takes six seconds first, longer than a daemon
/// gives a question; and on `please-get-stuck`, it says it runs `luneta
/// here`, writes its pid in `stuck` there, and sleeps for a minute.
fn claude_that_takes_turns(host: &Host) {
    let home = host.home.path().display();
    let text = format!(
        r#"cat > "{home}/words"
{{ echo "cwd=$(pwd -P)"; echo "pane=${{LUNETA_PANE-unset}}"; echo "away=${{LUNETA_AWAY-unset}}"
echo "turn=${{LUNETA_TURN-unset}}"; echo "before=${{LUNETA_TURN_BEFORE-unset}}"
resumed=none; for arg; do [ "$after" = --resume ] && resumed=$arg; after=$arg; done
echo "resumed=$resumed"; echo "model=$2 $3"
}} > "{home}/told"
echo '{{"type":"system","subtype":"init","session_id":"7c22"}}'
ran='{{"type":"assistant","parent_tool_use_id":null,"message":{{"content":[{{"type":"tool_use",'\
'"id":"toolu_1","name":"Bash","input":{{"command":"luneta here"}}}}]}}}}'
case "$(cat "{home}/words")" in
    *please-fail*)
        echo '{{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}}'
        exit 1 ;;
    *please-be-slow*) sleep 6 ;;
    *please-get-stuck*) echo "$ran"; echo $$ > "{home}/stuck"; exec sleep 60 ;;
esac
echo "$ran"
echo '{{"type":"result","subtype":"success","is_error":false,"result":"You are in work:2."}}'
"#
    );
    script(&host.zellij.bin().join("claude"), &text);
}

/// What `claude` there was told as it ran, by `claude_that_takes_turns`, a
/// line of each, and the words it was told on its stdin.
fn told(host: &Host) -> (Vec<String>, String) {
    let told = fs::read_to_string(host.home.path().join("told")).unwrap();
    let words = fs::read_to_string(host.home.path().join("words")).unwrap();
    (told.lines().map(str::to_owned).collect(), words)
}

/// The steps of the turn `request` asks of `daemon`, as it hands them on,
/// and how the turn ended.
async fn turn(
    daemon: &mut DaemonClient<Channel>,
    request: AskRequest,
) -> (Vec<Step>, Result<(), Status>) {
    let mut steps = match daemon.ask(request).await {
        Ok(steps) => steps.into_inner(),
        Err(status) => return (Vec::new(), Err(status)),
    };
    let mut heard = Vec::new();
    loop {
        match steps.message().await {
            Ok(Some(step)) => heard.extend(Step::of(step).expect("a step")),
            Ok(None) => return (heard, Ok(())),
            Err(status) => return (heard, Err(status)),
        }
    }
}

/// A turn of the ask box asked of `on`, with `words`, in `cwd`, for the user
/// where `user` says, the first of its talk or else one that goes on with
/// the session `7c22`.
fn ask(on: &str, cwd: Option<&Path>, words: &str, user: User, first: bool) -> AskRequest {
    let resumed = (!first).then(|| SessionId::parse("7c22").unwrap());
    let turn = Turn::new(255, (!first).then_some(10)).unwrap();
    let row = "The row highlighted is session work.".to_owned();
    let asked = Asked::new(cwd.map(Path::to_owned), words, resumed, row, user, turn).unwrap();
    asked.asked(Some(On { destination: on.to_owned() }))
}

/// A turn of the ask box asked of another machine runs `claude` there: in
/// the directory it names there, or in the home of the daemon's user there,
/// for the user in the pane it names there, or on another machine, in the
/// turn it names, on the session it resumes. The daemon there hands on each
/// step `claude` says as it says it, and the turn's end: well, or why not.
/// A question that hangs up gives up the turn, and `claude` there is
/// killed. One for a machine this daemon does not reach, or for no turn,
/// runs nothing.
#[test]
fn a_turn_of_the_ask_box_runs_on_the_machine_it_is_for() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    claude_that_takes_turns(&far);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());
    let dir = far.home.path().join("w");
    fs::create_dir(&dir).unwrap();
    let (dir_there, home_there) =
        (dir.canonicalize().unwrap(), far.home.path().canonicalize().unwrap());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let work = User::Pane { session: SessionName::running("work").unwrap(), pane: 2 };
        let first = ask("far", Some(&dir), "where am I", work, true);
        let (steps, ended) = turn(&mut daemon, first).await;
        let ran = Step::Ran { id: "toolu_1".to_owned(), command: "luneta here".to_owned() };
        let session = Step::Session(SessionId::parse("7c22").unwrap());
        let said = Step::Said("You are in work:2.".to_owned());
        assert_eq!(steps, [session.clone(), ran.clone(), said.clone()]);
        ended.unwrap();
        let (told_there, words) = told(&far);
        assert_eq!(told_there, [
            format!("cwd={}", dir_there.display()),
            "pane=work:2".to_owned(),
            "away=unset".to_owned(),
            "turn=00000000000000ff".to_owned(),
            "before=unset".to_owned(),
            "resumed=none".to_owned(),
            "model=--model sonnet".to_owned(),
        ]);
        assert_eq!(words, "[luneta: the user is in pane work:2.]\n\nwhere am I");

        let (steps, ended) = turn(&mut daemon, ask("far", None, "yes", User::Away, false)).await;
        assert_eq!(steps, [session.clone(), ran.clone(), said]);
        ended.unwrap();
        let (told_there, words) = told(&far);
        assert_eq!(told_there[..6], [
            format!("cwd={}", home_there.display()),
            "pane=unset".to_owned(),
            "away=1".to_owned(),
            "turn=00000000000000ff".to_owned(),
            "before=000000000000000a".to_owned(),
            "resumed=7c22".to_owned(),
        ]);
        assert!(words.starts_with("[luneta: the user is on another machine just now"), "{words}");

        let failing = ask("far", None, "please-fail", User::Untold, true);
        let (steps, ended) = turn(&mut daemon, failing).await;
        assert_eq!(steps, std::slice::from_ref(&session));
        let failed = ended.unwrap_err();
        assert_eq!((failed.code(), failed.message()), (Code::FailedPrecondition, "Not logged in"));

        // Hung up as `claude` there runs, the turn is given up there.
        let stuck = ask("far", None, "please-get-stuck", User::Untold, true);
        let mut steps = daemon.ask(stuck).await.unwrap().into_inner();
        for step in [session, ran] {
            assert_eq!(steps.message().await.unwrap().map(Step::of), Some(Ok(Some(step))));
        }
        let stuck = fs::read_to_string(far.home.path().join("stuck")).unwrap();
        let stuck = Pid::from_raw(stuck.trim().parse().unwrap()).unwrap();
        assert!(test_kill_process(stuck).is_ok());
        drop(steps);
        // The runtime sends the hang-up as it runs on.
        let deadline = Instant::now() + Duration::from_secs(10);
        while test_kill_process(stuck).is_ok() {
            assert!(Instant::now() < deadline, "claude there is never killed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let invalid =
            |status: Status| assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        let x = || ask("far", None, "x", User::Untold, true);
        let relative = AskRequest { cwd: Some("w".to_owned()), ..x() };
        invalid(turn(&mut daemon, relative).await.1.unwrap_err());
        let blank = AskRequest { words: " ".to_owned(), ..x() };
        invalid(turn(&mut daemon, blank).await.1.unwrap_err());
        let forged = AskRequest { resumed: Some("--debug".to_owned()), ..x() };
        invalid(turn(&mut daemon, forged).await.1.unwrap_err());
        let nowhere = turn(&mut daemon, ask("nowhere", None, "x", User::Untold, true)).await;
        assert_eq!(nowhere.1.unwrap_err().code(), Code::Unavailable);
        assert!(!here.home.path().join("told").exists());
    });
}

/// A turn of the ask box there takes as long as `claude` there does: longer
/// than a daemon gives a question, which bounds only the time to the
/// stream's start, and quiet all the while.
#[test]
fn a_turn_of_the_ask_box_there_takes_as_long_as_claude_does() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    claude_that_takes_turns(&far);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let started = Instant::now();
        let slow = ask("far", None, "please-be-slow", User::Untold, true);
        let (steps, ended) = turn(&mut daemon, slow).await;
        ended.unwrap();
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert!(started.elapsed() >= Duration::from_secs(6), "{:?}", started.elapsed());
    });
}

/// Where a key goes for `pasted` on the machine `on`, as `daemon` hands on
/// the one answer its stream holds, by places of the panes `pasted` says; or
/// why there is none.
/// The directories zoxide knows on another machine are listed there, best
/// first, each named for the session a luneta makes there now: past the
/// name of a session that runs there. What is in one is what eza lists
/// there. A directory by no absolute path is invalid; a machine the daemon
/// does not reach lists nothing.
#[test]
fn another_machine_s_directories_are_listed_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let dir = tempfile::tempdir().unwrap();
    let (work, notes) = (dir.path().join("work"), dir.path().join("notes"));
    fs::create_dir(&work).unwrap();
    fs::create_dir(&notes).unwrap();
    let (work, notes) = (work.display().to_string(), notes.display().to_string());
    let lists = |dirs: &[&str]| {
        dirs.iter().fold(String::new(), |mut script, dir| {
            writeln!(script, "echo '{dir}'").unwrap();
            script
        })
    };
    script(&far.zellij.bin().join("zoxide"), &lists(&[&work, &notes]));
    script(&here.zellij.bin().join("zoxide"), &lists(&["/here/only"]));
    script(&far.zellij.bin().join("eza"), "echo \"far: $*\"\n");
    script(&here.zellij.bin().join("eza"), "echo \"here: $*\"\n");
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let listed = daemon.list_dirs(ListDirsRequest { on: on("far") }).await.unwrap();
        let named: Vec<(String, String)> =
            listed.into_inner().dirs.into_iter().map(|dir| (dir.path, dir.name)).collect();
        let dirs = [(work.clone(), "work-2".to_owned()), (notes.clone(), "notes".to_owned())];
        assert_eq!(named, dirs);
        let nowhere = daemon.list_dirs(ListDirsRequest { on: on("nowhere") }).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::Unavailable, "{nowhere:?}");

        let contents = |dir: &str| ListContentsRequest { on: on("far"), dir: dir.to_owned() };
        let listed = daemon.list_contents(contents(&notes)).await.unwrap().into_inner().listed;
        assert!(listed.starts_with("far: ") && listed.ends_with(&format!(" -- {notes}\n")));
        let relative = daemon.list_contents(contents("notes")).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");
    });
}

async fn pasted_on(
    daemon: &mut DaemonClient<Channel>,
    on: &str,
    pasted: &Pasted,
) -> Result<Ready<usize>, Status> {
    let request = pasted.asked(Some(On { destination: on.to_owned() }));
    let mut answer = daemon.paste(request).await?.into_inner();
    let ready = answer.message().await?.expect("an answer");
    assert_eq!(answer.message().await?, None, "the stream ends after the answer");
    Ok(Ready::of(ready).expect("an answer luneta reads"))
}

/// A link pasted on a row of another machine is found there: gh there asks
/// GitHub of the issue, git there reads the repository its panes work in,
/// and `luneta worktree` there makes the issue's branch's worktree, beside
/// the lunetad there. The key goes to a tab of the session of the pane
/// that works in the repository, by its place among those luneta said; or,
/// with none, to a session to make in the clone zoxide knows there, named
/// for it as the daemon there names it then. A paste asked of a pane by no
/// absolute path is refused, and one for a machine the daemon does not
/// reach runs nothing.
#[test]
fn a_link_pasted_on_another_machine_s_row_is_found_and_made_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    far.gh.issue(7, "https://github.com/o/n/issues/7");
    let scratch = tempfile::tempdir().unwrap();
    // As git says the worktree, by its real path, as on macOS, where the
    // temporary folder is through a link.
    let scratch = scratch.path().canonicalize().unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    // The issue's repository on GitHub, by a remote of the clone, which git
    // takes for `up`.
    let bare = clone.join(".bare");
    let github = "git@github.com:o/n.git";
    git(&bare, &["remote", "add", "upstream", github]);
    git(&bare, &["config", &format!("url.{}.insteadOf", up.display()), github]);
    let main = clone.join("main").display().to_string();
    script(&far.zellij.bin().join("zoxide"), &format!("echo '{main}'\n"));
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let link = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let worked = |dir: &str| links::Worked { dir: dir.to_owned(), pull: None, here: false };
        let session = Pasted::new(link.clone(), vec![worked("/nowhere/at/all"), worked(&main)]);
        let ready = pasted_on(&mut daemon, "far", &session).await.unwrap();
        let worktree = clone.join("issue-7").display().to_string();
        let branch = Some("issue-7".to_owned());
        assert_eq!(ready, Ready { at: At::Session(1), worktree: worktree.clone(), branch });
        assert!(clone.join("issue-7").join(".git").is_file());
        let viewed = "issue view 7 --repo github.com/o/n --json url --jq .url";
        assert_eq!(far.gh.issues(), [viewed]);
        assert!(here.gh.issues().is_empty());

        let made = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), Vec::new())).await;
        let Ready { at: At::Clone { cwd, name }, worktree: again, .. } = made.unwrap() else {
            panic!("no session to make");
        };
        assert_eq!((cwd, name.as_str(), again), (clone.display().to_string(), "n", worktree));

        let relative = Pasted::new(link.clone(), vec![worked("n/main")]);
        let relative = pasted_on(&mut daemon, "far", &relative).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");
        let nowhere = pasted_on(&mut daemon, "nowhere", &session).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::Unavailable, "{nowhere:?}");
        assert_eq!(far.gh.issues().len(), 2);
    });
}

/// A tab asked to open alone in a session of another machine takes the
/// place there of the tabs zellij made the session with, which go once it
/// has opened, where the session started then or after, by that machine's
/// clock. One that started before keeps them, and the answer says so. None
/// opens in a session no client is in, alone or not: zellij would give it
/// no room for its panes.
#[test]
fn a_tab_there_opens_alone_in_a_session_made_since() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _work = far.serve("work");
    let laid = |id: u32, tab_id: u32| {
        serde_json::json!({
            "id": id, "is_plugin": false, "is_selectable": true, "is_suppressed": false,
            "is_floating": false, "tab_id": tab_id,
        })
    };
    far.zellij.list("work", &serde_json::json!([laid(10, 1), laid(11, 2)]));
    far.zellij.clients("work", &[(1, "terminal_10")]);
    far.zellij.opens_into("work", &serde_json::json!([laid(10, 1), laid(11, 2), laid(12, 3)]));
    far.zellij.list("idle", &serde_json::json!([laid(13, 1)]));
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let alone_in = |session: &str, since: SystemTime| OpenRequest {
            on: Some(On { destination: "far".to_owned() }),
            session: session.to_owned(),
            cwd: "/w/n/issue-7".to_owned(),
            command: Vec::new(),
            opens: Some(Opens::Tab(open_request::Tab {
                name: Some("#7".to_owned()),
                alone_since: Some(since.into()),
            })),
        };
        let alone = |since| alone_in("work", since);
        let made = daemon.open(alone(SystemTime::UNIX_EPOCH)).await.unwrap().into_inner();
        assert_eq!((made.terminal, made.notes), (12, Vec::new()));
        let closed = [1, 2].map(|tab| format!("--session work action close-tab-by-id {tab}"));
        assert_eq!(far.zellij.closed(), closed);
        let later = SystemTime::now() + Duration::from_hours(1);
        let kept = daemon.open(alone(later)).await.unwrap().into_inner();
        let notes = vec!["work ran already, and keeps its tabs".to_owned()];
        assert_eq!((kept.terminal, kept.notes), (12, notes));
        assert_eq!(far.zellij.closed(), closed);
        assert!(here.zellij.closed().is_empty());
        let idle = daemon.open(alone_in("idle", SystemTime::UNIX_EPOCH)).await.unwrap_err();
        assert_eq!(idle.code(), Code::FailedPrecondition, "{idle:?}");
        assert!(idle.message().contains("no client is in idle"), "{idle:?}");
        assert_eq!(far.zellij.closed(), closed);
    });
}
