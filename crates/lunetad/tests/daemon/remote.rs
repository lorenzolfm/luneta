//! The other machines a daemon watches, as its hosts file names them, through
//! `ssh <destination> luneta bridge`: here, through an ssh of the test's own,
//! which runs the bridge of another host of the test's.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{symlink, DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::remote::unreached::{self, Stopped};
use lunetad::api::remote::{self, Reached};
use lunetad::api::keep_image_request::{Image, Part};
use lunetad::api::keep_checks_response::{failed, Failed};
use lunetad::api::keep_printed_response::{self, File};
use lunetad::api::{
    close_request, closes_response, list_exited_response, other_tabs_response,
    turn_into_panes_response, turn_into_tab_response,
};
use lunetad::api::list_skills_response::Skill;
use lunetad::api::open_request::{self, Opens};
use lunetad::api::refuge_response::{Ended, Went};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::{
    self, AskRequest, CheckNameRequest, Client, CloseRequest, ClosesRequest, DeleteExitedRequest,
    DumpScreenRequest, GoRequest, KeepChecksRequest, KeepImageRequest, KeepPrintedRequest,
    KillRequest,
    ListClientsRequest,
    ListContentsRequest, ListDirsRequest, ListExitedRequest, ListSkillsRequest,
    NameSessionRequest, On, OpenRequest, OtherTabsRequest, PasteRequest, RefugeRequest,
    RenameSessionRequest, StartRequest, TranscriptEndsRequest, TurnIntoPanesRequest,
    TurnIntoTabRequest, WatchRequest,
};
use lunetad::asks::{Asked, Step, Turn, User};
use lunetad::images::{self, Png};
use lunetad::links::{self, At, Link, Pasted, Readied, Ready, Unready, Wanted, Work};
use lunetad::transcripts::SessionId;
use lunetad::{Directory, SessionName};
use rustix::process::{kill_process, test_kill_process, Pid, Signal};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::demand::{stop, Detached};
use crate::host::{
    answered, eventually, folder_in, script, shared_script, start, start_as, Host, Vars, DROPPED,
    LUNETA, LUNETAD,
};
use crate::worktree::{cloned_as_wt, git};

/// An `ssh` of a test's own, in a folder to put first on the `PATH` of a
/// host's daemon, that reaches the test's other hosts by the destinations it
/// was given for them with `reaches`: it runs `luneta bridge` with their
/// variables, as ssh runs it on the machine it reaches. Of a destination it
/// was given none for, it says it cannot resolve it, and ends as ssh does
/// then, with 255. Nor does it run anything on a terminal, which would mangle
/// what the daemons say, or where it could ask for a password, which no one
/// would answer: without `-T` and `BatchMode=yes`, it ends so too. It notes
/// each time it is run to a destination, for `runs`, each time it could not
/// resolve one, for `unresolved`, and the pid of each bridge it runs, for
/// `bridges`. Run with `-t`, as luneta runs a client of a
/// session there, it runs nothing: it notes the command, for `clients`, and
/// ends well, as a client that detached does, or as one zellij there dropped
/// does, as many as a test had it `drops_clients`, or as ssh does that failed,
/// where a test had it `refuse` clients.
pub struct FakeSsh {
    to: PathBuf,
}

impl FakeSsh {
    /// Writes the `ssh` in `bin`.
    pub fn new(bin: &Path) -> Self {
        let ssh = FakeSsh { to: bin.join("ssh-to") };
        fs::create_dir(&ssh.to).unwrap();
        let text = r#"to="${0%/*}/ssh-to"
quiet= batch= client=
while [ $# -gt 0 ]; do
    case "$1" in
        -T) quiet=yes; shift ;;
        -t) client=yes; shift ;;
        -o) [ "$2" = BatchMode=yes ] && batch=yes; shift 2 ;;
        -*) echo "this ssh knows no $1" >&2; exit 255 ;;
        *) break ;;
    esac
done
if [ -n "$client" ] && [ -z "$quiet" ]; then
    destination="$1"
    shift
    echo "$*" >> "$to/$destination.clients"
    if [ -f "$to/$destination.refused" ]; then
        echo "ssh: connect to host $destination port 22: Connection refused" >&2
        exit 255
    fi
    if [ -f "$to/$destination.drops" ]; then
        drops=$(cat "$to/$destination.drops")
        if [ "$drops" -gt 0 ]; then
            echo $((drops - 1)) > "$to/$destination.drops"
            cat "$to/dropped"
        fi
    fi
    exit 0
fi
if [ -z "$quiet" ] || [ -z "$batch" ]; then
    echo "this ssh runs nothing on a terminal, or where it could ask" >&2
    exit 255
fi
destination="$1"
shift
echo run >> "$to/$destination.runs"
if [ ! -x "$to/$destination" ]; then
    echo unresolved >> "$to/$destination.unresolved"
    echo "ssh: Could not resolve hostname $destination: Name or service not known" >&2
    exit 255
fi
if [ "$*" != "luneta bridge" ]; then
    echo "this ssh runs luneta bridge alone, and was asked: $*" >&2
    exit 255
fi
echo $$ >> "$to/$destination.bridges"
exec "$to/$destination"
"#;
        shared_script(&bin.join("ssh"), text);
        ssh
    }

    /// Has ssh reach the host whose variables are `vars` as `destination`.
    pub fn reaches(&self, destination: &str, vars: Vars) {
        script(&self.to.join(destination), &bridge(&vars));
    }

    /// Has ssh reach `destination` as `reaches` does, `secs` after it is run,
    /// as over a slow link.
    fn reaches_slowly(&self, destination: &str, vars: Vars, secs: u32) {
        script(&self.to.join(destination), &format!("sleep {secs}\n{}", bridge(&vars)));
    }

    /// Has ssh wait on `destination` for good, saying nothing, as it does on
    /// a machine that is off.
    pub fn waits_on(&self, destination: &str) {
        script(&self.to.join(destination), "exec sleep 60\n");
    }

    /// Has a client of a session on `destination` fail from now on, as ssh
    /// does that loses the machine.
    pub fn refuses_clients(&self, destination: &str) {
        fs::write(self.to.join(format!("{destination}.refused")), "").unwrap();
    }

    /// Has the next `clients` of a session on `destination` end as one zellij
    /// there dropped does.
    pub fn drops_clients(&self, destination: &str, clients: u32) {
        fs::write(self.to.join("dropped"), DROPPED).unwrap();
        fs::write(self.to.join(format!("{destination}.drops")), clients.to_string()).unwrap();
    }

    /// The command of each client of a session on `destination` that was
    /// started, in the order they were.
    pub fn clients(&self, destination: &str) -> Vec<String> {
        let clients = fs::read_to_string(self.to.join(format!("{destination}.clients")));
        clients.unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// Has ssh to `destination` end with `code`, before it reaches lunetad.
    fn ends_with(&self, destination: &str, code: u8) {
        script(&self.to.join(destination), &format!("exit {code}\n"));
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

    /// How many times ssh to `destination` could not resolve it.
    fn unresolved(&self, destination: &str) -> usize {
        let unresolved = fs::read_to_string(self.to.join(format!("{destination}.unresolved")));
        unresolved.unwrap_or_default().lines().count()
    }

    /// The pids of the bridges ssh has run to `destination`, in turn.
    fn bridges(&self, destination: &str) -> Vec<u32> {
        let path = self.to.join(format!("{destination}.bridges"));
        let bridges = fs::read_to_string(path).unwrap_or_default();
        bridges.lines().map(|pid| pid.parse().unwrap()).collect()
    }

    /// Has what `destination`, as `reaches` has it, says come back through a
    /// `cat` of its own, for `stalls_answers` to stop.
    fn answers_through_a_cat(&self, destination: &str) {
        let bridge = self.to.join(format!("{destination}.bridge"));
        fs::rename(self.to.join(destination), &bridge).unwrap();
        let cat = self.to.join(format!("{destination}.cat"));
        let text = format!(
            "{} | sh -c 'echo $$ > \"$1\"; exec cat' sh {}\n",
            quoted(bridge.to_str().unwrap()),
            quoted(cat.to_str().unwrap()),
        );
        script(&self.to.join(destination), &text);
    }

    /// Stops what `destination` says on its way back, as a link that goes
    /// quiet does, while what is asked of it goes on reaching it, until what
    /// this returns goes.
    fn stalls_answers(&self, destination: &str) -> StalledAnswers {
        let cat = fs::read_to_string(self.to.join(format!("{destination}.cat"))).unwrap();
        let cat = Pid::from_raw(cat.trim().parse().unwrap()).unwrap();
        kill_process(cat, Signal::STOP).unwrap();
        StalledAnswers(cat)
    }
}

/// The `cat` a machine's answers come back through, stopped. It goes on as
/// this goes, so that the bridge can end.
struct StalledAnswers(Pid);

impl Drop for StalledAnswers {
    fn drop(&mut self) {
        // It ended, with the bridge the daemon gave up on.
        let _ = kill_process(self.0, Signal::CONT);
    }
}

/// What runs `luneta bridge` as the host whose variables are `vars`.
fn bridge(vars: &Vars) -> String {
    let bridge = vars.command(LUNETA);
    let (mut unset, mut set) = (String::new(), String::new());
    for (name, value) in bridge.get_envs() {
        let name = name.to_str().unwrap();
        match value {
            None => write!(unset, " -u {name}").unwrap(),
            Some(value) => write!(set, " {name}={}", quoted(value.to_str().unwrap())).unwrap(),
        }
    }
    format!("exec env{unset}{set} {} bridge\n", quoted(LUNETA))
}

/// `text` as the shell reads it as one word, whatever is in it.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Has `host`'s daemon watch the machines `text` names, as its hosts file,
/// written whole before the daemon can read it.
pub fn hosts(host: &Host, text: &str) {
    let dir = host.home.path().join(".local/state/luneta");
    fs::create_dir_all(&dir).unwrap();
    let writing = dir.join("hosts.writing");
    fs::write(&writing, text).unwrap();
    fs::rename(writing, dir.join("hosts")).unwrap();
}

/// The host of the machine `host`'s daemon watches as `destination`, once
/// it has reached it and `wanted` takes what it says.
pub fn reached(
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
    let far = ["far/", "  work  (no panes listed yet)", "  play  (no panes listed yet)"];
    assert_eq!(lines[1..4], far, "{said}");
    let unread = "(lunetad could not read all of luneta's hosts file: its log says why)";
    assert_eq!(lines[4..], [unread], "{said}");
}

/// An agent on another machine is heard where you sit, as the agents are.
#[test]
fn what_another_machine_is_told_to_say_comes_with_its_host() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let said = answered(far.vars().command(LUNETA).args(["say", "over here"]));
    assert!(said.status.success(), "{}", String::from_utf8_lossy(&said.stderr));

    let far_host = reached(&here, "far", |far| !far.sayings.is_empty());
    assert_eq!(far_host.sayings.len(), 1);
    assert_eq!(here.host().sayings, [], "it is the other machine's to say");
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

/// A question for a machine whose daemon makes way for a newer one, while
/// ssh to it runs again, is handed on to the newer one, as the machine's
/// host stands meanwhile, rather than refused.
#[test]
fn a_question_while_a_machine_is_reached_again_is_handed_on() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    ssh.reaches_slowly("far", far.vars(), 2);
    let runs = ssh.runs("far");
    drop(far_daemon);
    eventually("ssh runs to far again", || (ssh.runs("far") > runs).then_some(()));
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let checked = runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let on = Some(On { destination: "far".to_owned() });
        let check = CheckNameRequest { on, name: "fresh".to_owned() };
        tokio::time::timeout(Duration::from_secs(10), daemon.check_name(check)).await
    });
    assert!(checked.as_ref().is_ok_and(Result::is_ok), "{checked:?}");
    assert_eq!(ssh.runs("far"), runs + 1);
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
            let asked = WatchRequest { after, kept: None, shown: false };
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
            let asked = daemon.watch(WatchRequest { after, kept: None, shown: false }).await;
            after = asked.expect("the daemon ended").into_inner().version;
        }
    });
}

/// A daemon that a client keeps keeps the daemons of the machines it watches
/// running, however long zellij there has had no session, as a client there
/// would: `luneta bridge` would else start one again each time one ended.
/// The test watches the one for longer than the other would run on alone.
pub fn a_daemon_a_client_keeps_keeps_the_machines_it_watches_running() {
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
pub fn two_daemons_that_watch_each_other_and_nothing_else_end() {
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

/// What `host`'s daemon answers a client that asks to go to a session on
/// `destination`, which it refuses, as it does not reach it.
fn refused_go(host: &Host, destination: &str) -> Status {
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let on = Some(On { destination: destination.to_owned() });
        let go = GoRequest { on, at: "work".to_owned(), to: "play".to_owned(), pane: None };
        daemon.go(go).await.unwrap_err()
    })
}

/// A question for a machine the daemon does not reach is refused, and has
/// ssh run to it again at once, where the daemon would else wait longer
/// each time ssh failed: 8 seconds after the fifth. The machine is reached
/// in moments, for the next question to find.
#[test]
fn a_question_for_a_machine_not_reached_runs_ssh_to_it_at_once() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    eventually("ssh fails to far five times", || {
        here.host();
        (ssh.unresolved("far") >= 5).then_some(())
    });

    ssh.reaches("far", far.vars());
    let asked = Instant::now();
    let refused = refused_go(&here, "far");
    assert_eq!(refused.code(), Code::Unavailable, "{refused:?}");
    reached(&here, "far", |_| true);
    let took = asked.elapsed();
    assert!(took < Duration::from_secs(5), "reached {took:?} after it was asked for");
    assert_eq!(ssh.runs("far"), 6);
}

/// The log says why a machine is unreached once, while each ssh to it fails
/// alike, and again as why changes, and how long it was unreached once it is
/// reached.
#[test]
fn the_log_says_why_a_machine_is_unreached_as_that_changes() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let mut daemon = start(here.vars()).unwrap();
    eventually("ssh fails to far twice", || {
        here.host();
        (ssh.runs("far") >= 2).then_some(())
    });
    ssh.ends_with("far", 7);
    let runs = ssh.runs("far");
    eventually("ssh fails to far twice more", || {
        here.host();
        (ssh.runs("far") >= runs + 2).then_some(())
    });
    ssh.reaches("far", far.vars());
    refused_go(&here, "far");
    reached(&here, "far", |_| true);

    let said = daemon.said_after(Duration::from_secs(1));
    let unreached: Vec<&String> =
        said.iter().filter(|line| line.contains("far: unreached: ")).collect();
    assert_eq!(unreached.len(), 2, "{said:?}");
    assert!(unreached[0].ends_with("(exit status: 255)"), "{said:?}");
    assert!(unreached[1].ends_with("(exit status: 7)"), "{said:?}");
    let reached = said.iter().filter(|line| line.contains("far: reached, after ")).count();
    assert_eq!(reached, 1, "{said:?}");
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
/// directory, what a pane's shell printed, which it keeps there, what a
/// terminal shows there, and the name a session for a directory takes
/// there, and to start it, and what closing a pane closes there, to close
/// it, and a session to kill, and the sessions that ended there, to let one
/// go of, and a session to rename, and the ends of transcripts in Claude
/// Code's folder there. Without a machine, it asks its own; of one it does
/// not reach, it says so. A question that names a directory by no absolute
/// path, an empty line typed, a screen of no session, a tab by an empty
/// name, a tab to close of no terminal, a session to let go of by no name, a
/// name zellij would make no session of, or a transcript by no session's
/// id, or more of the transcripts than a brief keeps, or none of them, is
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
    far.zellij.list("turning", &serde_json::json!([tab, beside]));
    far.zellij.clients("turning", &[(1, "terminal_4")]);
    let there = serde_json::json!({
        "id": 4, "is_plugin": false, "is_selectable": true, "is_suppressed": false,
        "is_floating": false, "tab_id": 6,
    });
    far.zellij.stacks_into("turning", 1, &serde_json::json!([there, beside]));
    far.zellij.dumps("work", "~> cargo test\nerror[E0308]\n~>\n");
    let job = "https://github.com/o/n/actions/runs/42/job/7";
    let run = |name: &str, conclusion: &str| serde_json::json!({
        "__typename": "CheckRun", "name": name, "workflowName": "ci", "status": "COMPLETED",
        "conclusion": conclusion, "detailsUrl": job,
    });
    let rollup = serde_json::json!([run("test", "FAILURE"), run("clippy", "SUCCESS")]);
    far.gh.checks(328, "OPEN", "abc", &rollup);
    far.gh.checks(329, "MERGED", "def", &rollup);
    far.gh.log(7, "test\tRun\terror[E0308]\n");
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
        assert_eq!(here.zellij.went(), [] as [String; 0]);

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
        assert_eq!(found.ended, Some(Ended::Kept(())));

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
        assert_eq!(here.zellij.opened(), [] as [String; 0]);

        let skills = dir.path().to_str().unwrap().to_owned();
        let skills = ListSkillsRequest { on: on("far"), dir: skills };
        let found = daemon.list_skills(skills).await.unwrap();
        let near =
            Skill { name: "near".to_owned(), description: "Near.".to_owned(), only_you: false };
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

        // The logs of a pull request's checks that failed are kept there, as
        // gh there fetches them, older or not, and none of one merged since.
        let checks = |url: &str| KeepChecksRequest { on: on("far"), url: url.to_owned() };
        let kept = daemon.keep_checks(checks("https://github.com/o/n/pull/328")).await;
        let kept = kept.unwrap().into_inner().message().await.unwrap().unwrap();
        assert_eq!((kept.head.as_str(), kept.pending, kept.passed), ("abc", 0, 1));
        let [Failed { name, link, log: Some(failed::Log::Kept(File { path, lines: 1, .. })) }] =
            kept.failed.as_slice()
        else {
            panic!("kept {kept:?}");
        };
        assert_eq!((name.as_str(), link.as_str()), ("ci / test", job));
        assert!(Path::new(&path).starts_with(far.runtime.path().canonicalize().unwrap()), "{path}");
        assert_eq!(fs::read_to_string(path).unwrap(), "test\tRun\terror[E0308]\n");
        let viewed = "pr view https://github.com/o/n/pull/328 --json \
                      state,headRefOid,headRepository,headRepositoryOwner,statusCheckRollup";
        let logs = "repos/o/n/actions/jobs/7/logs";
        let fetched = format!("api --hostname github.com --allow-escape-sequences {logs}");
        let asked = [viewed, fetched.as_str()];
        assert_eq!(far.gh.checked(), asked);
        assert_eq!(here.gh.checked(), [] as [String; 0]);
        invalid(daemon.keep_checks(checks("https://github.com/o/n/issues/328")).await.unwrap_err());
        let unknown = daemon.keep_checks(checks("https://github.com/o/n/pull/9")).await;
        let unknown = unknown.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(unknown.code(), Code::FailedPrecondition, "{unknown:?}");
        let merged = daemon.keep_checks(checks("https://github.com/o/n/pull/329")).await;
        let merged = merged.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(merged.code(), Code::FailedPrecondition, "{merged:?}");
        assert_eq!(merged.message(), "https://github.com/o/n/pull/329 is merged now");
        let fetching = || {
            let checked = far.gh.checked().into_iter();
            checked.filter(|asked| asked.starts_with("api")).collect::<Vec<_>>()
        };
        assert_eq!(fetching(), std::slice::from_ref(&fetched));
        far.gh.old();
        let kept = daemon.keep_checks(checks("https://github.com/o/n/pull/328")).await;
        let kept = kept.unwrap().into_inner().message().await.unwrap().unwrap();
        let [Failed { log: Some(failed::Log::Kept(File { path, .. })), .. }] =
            kept.failed.as_slice()
        else {
            panic!("kept {kept:?}");
        };
        assert_eq!(fs::read_to_string(path).unwrap(), "test\tRun\terror[E0308]\n");
        let again = format!("api --hostname github.com {logs}");
        assert_eq!(fetching(), [fetched.clone(), fetched, again]);

        // A screen there is dumped there: of the terminal asked, or else of
        // the one a client coming to the session would be on, as zellij there
        // lists its panes.
        let dump = |pane| DumpScreenRequest { on: on("far"), session: "work".to_owned(), pane };
        let asked = daemon.dump_screen(dump(Some(2))).await.unwrap().into_inner();
        let shown = daemon.dump_screen(dump(None)).await.unwrap().into_inner();
        let screen = "~> cargo test\nerror[E0308]\n~>\n";
        assert_eq!((asked.pane, asked.screen.as_str()), (2, screen));
        assert_eq!((shown.pane, shown.screen.as_str()), (4, screen));
        let nameless = DumpScreenRequest { session: String::new(), ..dump(None) };
        invalid(daemon.dump_screen(nameless).await.unwrap_err());

        // A directory named for a session that runs there takes the name after.
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        let work = work.to_str().unwrap().to_owned();
        let before = SystemTime::now();
        let named = NameSessionRequest { on: on("far"), dir: Some(work.clone()) };
        let named = daemon.name_session(named).await.unwrap().into_inner();
        assert_eq!((named.name.as_str(), named.dir.as_deref()), ("work-2", Some(work.as_str())));
        // Just before the daemon there read the names, by its clock.
        let named_at = named.named_at.map(|at| SystemTime::try_from(at).unwrap());
        assert!(named_at.is_some_and(|at| at >= before && at <= SystemTime::now()), "{named_at:?}");
        let relative = NameSessionRequest { on: on("far"), dir: Some("work".to_owned()) };
        invalid(daemon.name_session(relative).await.unwrap_err());
        // With no directory, it is named there for the home of the daemon's
        // user there, which is not this machine's.
        let homes = [(on("far"), &far), (None, &here)];
        for (on, host) in homes {
            let home = NameSessionRequest { on, dir: None };
            let named = daemon.name_session(home).await.unwrap().into_inner();
            let name = host.home.path().file_name().and_then(|name| name.to_str());
            assert_eq!(Some(named.name.as_str()), name);
            assert_eq!(named.dir.as_deref(), host.home.path().to_str());
        }
        let (at, to, cwd) = ("work".to_owned(), named.name.clone(), Some("work".to_owned()));
        invalid(daemon.start(StartRequest { on: on("far"), at, to, cwd }).await.unwrap_err());
        let (at, cwd) = ("work".to_owned(), Some(work.clone()));
        daemon.start(StartRequest { on: on("far"), at, to: named.name, cwd }).await.unwrap();
        let started = format!("--session work action switch-session work-2 --cwd {work}");
        assert_eq!(far.zellij.went().last(), Some(&started));
        assert_eq!(here.zellij.went(), [] as [String; 0]);

        // A name is checked there: one a session there runs by is taken, and
        // another is free since the daemon there read the names, by its clock.
        let check = |on, name: &str| CheckNameRequest { on, name: name.to_owned() };
        let asked = SystemTime::now();
        let free = daemon.check_name(check(on("far"), "fresh")).await.unwrap().into_inner();
        let named_at = free.named_at.map(|at| SystemTime::try_from(at).unwrap());
        assert!(named_at.is_some_and(|at| at >= asked && at <= SystemTime::now()), "{named_at:?}");
        let runs = daemon.check_name(check(on("far"), "work")).await.unwrap_err();
        assert_eq!(runs.code(), Code::FailedPrecondition, "{runs:?}");
        assert!(runs.message().contains("work runs already"), "{runs:?}");
        daemon.check_name(check(None, "work")).await.unwrap();
        invalid(daemon.check_name(check(on("far"), "a/b")).await.unwrap_err());

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
        let whole = closes_response::Closes::Session(());
        assert_eq!(idle, Some(whole));
        // What it closes goes while zellij lists it as the close says: a pane
        // another holds its tab open beside, and a tab with its terminals.
        let close = |session: &str, closes| {
            let (session, started_at) = (session.to_owned(), Some(SystemTime::UNIX_EPOCH.into()));
            CloseRequest { on: on("far"), session, closes: Some(closes), started_at }
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

        // A tab there turns into panes of another there, as zellij there lists
        // them.
        let tabs = OtherTabsRequest { on: on("far"), session: "turning".to_owned(), pane: 4 };
        let tabs = daemon.other_tabs(tabs).await.unwrap().into_inner().tabs;
        assert_eq!(tabs, [other_tabs_response::Tab { pane: 5, name: String::new() }]);
        let session = || "turning".to_owned();
        let started_at = Some(SystemTime::UNIX_EPOCH.into());
        let turn = TurnIntoPanesRequest {
            on: on("far"),
            session: session(),
            pane: 4,
            beside: 5,
            started_at,
        };
        let came = daemon.turn_into_panes(turn).await.unwrap().into_inner().came;
        let done = turn_into_panes_response::Came::Done(());
        assert_eq!(came, Some(done));
        let stacked = "--session turning action stack-panes -- terminal_9 terminal_4";
        assert_eq!(far.zellij.turned(), [stacked]);
        assert_eq!(here.zellij.turned(), [] as [String; 0]);
        let stand_in = "--session turning action close-pane --pane-id terminal_9";
        assert_eq!(far.zellij.closed()[2], stand_in);

        // A pane there that a client has to come for says so.
        let session = "held".to_owned();
        let turn = TurnIntoTabRequest { on: on("far"), session, pane: 4, started_at };
        let came = daemon.turn_into_tab(turn).await.unwrap().into_inner().came;
        assert_eq!(came, Some(turn_into_tab_response::Came::NoClient(())));

        // A session there is killed by when it started, as its daemon lists
        // it, and deleted.
        let kill = |started_at| {
            KillRequest { on: on("far"), session: "work".to_owned(), started_at }
        };
        invalid(daemon.kill(kill(None)).await.unwrap_err());
        // What closes in a session there goes by when it started too.
        let held = |started_at| CloseRequest {
            on: on("far"),
            session: "work".to_owned(),
            closes: Some(pane(4)),
            started_at,
        };
        invalid(daemon.close(held(None)).await.unwrap_err());
        let since = Some(SystemTime::UNIX_EPOCH.into());
        let stays = daemon.close(held(since)).await.unwrap_err();
        assert!(stays.message().contains("a session made since"), "{stays:?}");
        daemon.kill(kill(work_started)).await.unwrap();
        assert_eq!(far.zellij.closed()[3..], ["delete-session --force work"]);
        assert_eq!(here.zellij.closed(), [] as [String; 0]);

        // The sessions that ended there are listed there, each by how long
        // before zellij listed it it was made, as zellij there says, and one
        // is let go of there.
        far.keep("old");
        let exited = |on| ListExitedRequest { on };
        let listed = daemon.list_exited(exited(on("far"))).await.unwrap().into_inner();
        let ago = Some(prost_types::Duration { seconds: 3723, nanos: 0 });
        let old = list_exited_response::Exited { name: "old".to_owned(), ago };
        assert_eq!(listed.exited, [old]);
        let listed = daemon.list_exited(exited(None)).await.unwrap().into_inner();
        assert_eq!(listed.exited, [] as [list_exited_response::Exited; 0]);
        let kept = daemon.check_name(check(on("far"), "old")).await.unwrap_err();
        assert_eq!(kept.code(), Code::FailedPrecondition, "{kept:?}");
        assert!(kept.message().contains("zellij keeps old, which ended"), "{kept:?}");
        let delete = |session: &str| {
            DeleteExitedRequest { on: on("far"), session: session.to_owned() }
        };
        daemon.delete_exited(delete("old")).await.unwrap();
        assert_eq!(far.zellij.closed()[4..], ["delete-session old"]);
        let listed = daemon.list_exited(exited(on("far"))).await.unwrap().into_inner();
        assert_eq!(listed.exited, [] as [list_exited_response::Exited; 0]);
        invalid(daemon.delete_exited(delete("")).await.unwrap_err());
        // A session there is renamed there, to a name zellij makes a session
        // of on any machine.
        let rename = |to: &str| {
            let session = "idle".to_owned();
            RenameSessionRequest { on: on("far"), session, to: to.to_owned() }
        };
        daemon.rename_session(rename("rest")).await.unwrap();
        assert_eq!(far.zellij.renamed(), ["--session idle action rename-session rest"]);
        invalid(daemon.rename_session(rename(&"long".repeat(16))).await.unwrap_err());
        assert_eq!(here.zellij.renamed(), [] as [String; 0]);

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

/// A PNG of `size` bytes, each saying where it is in the image.
fn png(size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((bytes.len()..size).map(|at| (at % 251) as u8));
    bytes
}

/// The first part of an image of `size` bytes kept on the machine `on`.
fn image_of(on: Option<On>, size: u64) -> KeepImageRequest {
    KeepImageRequest { part: Some(Part::Image(Image { on, size })) }
}

/// A part of the bytes of an image.
fn bytes_of(bytes: &[u8]) -> KeepImageRequest {
    KeepImageRequest { part: Some(Part::Bytes(bytes.to_vec())) }
}

/// What `daemon` answers `parts`, the question that keeps an image: the one
/// path its stream holds, or why there is none.
async fn kept_image(
    daemon: &mut DaemonClient<Channel>,
    parts: Vec<KeepImageRequest>,
) -> Result<String, Status> {
    let mut answer = daemon.keep_image(tokio_stream::iter(parts)).await?.into_inner();
    let kept = answer.message().await?.expect("an answer");
    assert_eq!(answer.message().await?, None, "the stream ends after the answer");
    Ok(kept.path)
}

/// An image goes through the bridge to the daemon of the machine it is for,
/// however much larger it is than the 4 MiB tonic takes in a message, and
/// that daemon keeps it in a file of its user's alone, by a name Claude Code
/// reads as an image's. The stream ends after the path. One asked of no
/// machine is kept by the daemon it is asked of.
///
/// An image that ends short of the size it said, or that is more than that,
/// or that does not start as a PNG does, keeps no file there; nor do one by
/// no size or by too few to start as one, one over the most that goes, one
/// with no first part to say what it is, or one for a machine this daemon
/// does not reach.
#[test]
fn an_image_is_kept_on_another_machine_through_the_bridge() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let images_in = |host: &Host| {
        let runtime = folder_in(host.runtime.path()).unwrap();
        let found = fs::read_dir(runtime.path().join("images"));
        found.map_or(0, Iterator::count)
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let image = png(5 * 1024 * 1024);
        let asked = Png::new(image.clone()).unwrap().asked(on("far"));
        let mut answer = daemon.keep_image(asked).await.unwrap().into_inner();
        let path = answer.message().await.unwrap().expect("an answer").path;
        assert_eq!(answer.message().await.unwrap(), None, "the stream ends after the answer");
        let far_runtime = far.runtime.path().canonicalize().unwrap();
        assert!(Path::new(&path).starts_with(&far_runtime), "{path}");
        assert_eq!(Path::new(&path).extension(), Some("png".as_ref()), "{path}");
        assert!(fs::read(&path).unwrap() == image, "the image came as it went");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!((images_in(&here), images_in(&far)), (0, 1));

        let small = png(16);
        let asked = Png::new(small.clone()).unwrap().asked(None);
        let mut answer = daemon.keep_image(asked).await.unwrap().into_inner();
        let path = answer.message().await.unwrap().expect("an answer").path;
        assert!(fs::read(&path).unwrap() == small);
        assert_eq!((images_in(&here), images_in(&far)), (1, 1));

        let refused = |status: Result<String, Status>, code: Code| {
            let status = status.unwrap_err();
            assert_eq!(status.code(), code, "{status:?}");
            status.message().to_owned()
        };
        let short = vec![image_of(on("far"), 16), bytes_of(&small[..8])];
        let why = refused(kept_image(&mut daemon, short).await, Code::FailedPrecondition);
        assert_eq!(why, "the image ended 8 bytes short");
        let long = vec![image_of(on("far"), 8), bytes_of(&small)];
        let why = refused(kept_image(&mut daemon, long).await, Code::FailedPrecondition);
        assert_eq!(why, "the image is larger than it said");
        let another = vec![image_of(on("far"), 8), image_of(None, 8)];
        let why = refused(kept_image(&mut daemon, another).await, Code::FailedPrecondition);
        assert_eq!(why, "a part after the first is not the image's bytes");
        let gif = vec![image_of(on("far"), 16), bytes_of(b"GIF89a, no PNG!!")];
        let why = refused(kept_image(&mut daemon, gif).await, Code::FailedPrecondition);
        assert_eq!(why, "the image is no PNG");
        assert_eq!(images_in(&far), 1);

        let unsized_ = vec![image_of(on("far"), 0)];
        refused(kept_image(&mut daemon, unsized_).await, Code::InvalidArgument);
        let too_few = vec![image_of(on("far"), 7), bytes_of(&small[..7])];
        let why = refused(kept_image(&mut daemon, too_few).await, Code::InvalidArgument);
        assert_eq!(why, "too few bytes to be a PNG: 7");
        let huge = vec![image_of(on("far"), images::BYTES_MAX + 1)];
        let why = refused(kept_image(&mut daemon, huge).await, Code::InvalidArgument);
        assert_eq!(why, "the image is 65 MiB, and the most that goes is 64 MiB");
        let nameless = vec![bytes_of(&small)];
        refused(kept_image(&mut daemon, nameless).await, Code::InvalidArgument);
        refused(kept_image(&mut daemon, Vec::new()).await, Code::InvalidArgument);
        let nowhere = vec![image_of(on("nowhere"), 16), bytes_of(&small)];
        refused(kept_image(&mut daemon, nowhere).await, Code::Unavailable);
        assert_eq!((images_in(&here), images_in(&far)), (1, 1));
    });
}

/// A daemon stopped as an image comes, as luneta stops an older one for a
/// newer, runs nothing on its way out. What had come of the image goes by no
/// name an image goes by: nothing takes it for a whole one.
#[test]
fn a_daemon_stopped_as_an_image_comes_leaves_no_image_of_it() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    let images = folder.path().join("images");
    let found = || -> Vec<PathBuf> {
        fs::read_dir(&images).map_or(Vec::new(), |found| found.flatten().map(|file| file.path()).collect())
    };
    let came = png(16);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut client = lunetad::connect(&folder).await.unwrap();
        let (send, parts) = tokio::sync::mpsc::channel(2);
        send.send(image_of(None, 1024)).await.unwrap();
        send.send(bytes_of(&came)).await.unwrap();
        let _answer = client.keep_image(ReceiverStream::new(parts)).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while found().iter().all(|file| fs::metadata(file).unwrap().len() < came.len() as u64) {
            assert!(Instant::now() < deadline, "what came of the image was never written");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop(daemon.pid());
        let ended = daemon.ended_within(Duration::from_secs(5)).expect("the daemon ends");
        assert_eq!(ended.signal(), Some(Signal::TERM.as_raw()), "it runs nothing on its way out");
        let png = |file: &PathBuf| file.extension() == Some("png".as_ref());
        assert!(!found().iter().any(png), "{:?}", found());
    });
}

/// The last file of a kind has no other kept after it to take it away, and it
/// holds what a screenshot shows, in memory on most Linuxes: a daemon that
/// starts takes away those past their day, as what a killed one left of an
/// image, and no younger one. What a link in the place of a folder leads to
/// is no file of luneta's, however old.
#[test]
fn a_daemon_that_starts_takes_away_the_files_past_their_day() {
    let (day, hour) = (Duration::from_hours(25), Duration::from_hours(1));
    let now = SystemTime::now();
    let folder = |path: PathBuf| {
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    };
    let file = |path: PathBuf, age: Duration| {
        fs::File::create(&path).unwrap().set_modified(now - age).unwrap();
        path
    };

    let host = Host::new();
    let runtime = folder(host.runtime.path().join("luneta"));
    let (images, printed) = (folder(runtime.join("images")), folder(runtime.join("printed")));
    let past = [
        file(images.join("past.png"), day),
        file(images.join("killed.png.part"), day),
        file(printed.join("past.txt"), day),
    ];
    let young = [file(images.join("young.png"), hour), file(printed.join("young.txt"), hour)];
    let _daemon = start(host.vars()).unwrap();
    eventually("the files past their day to go", || {
        past.iter().all(|file| !file.exists()).then_some(())
    });
    assert!(young.iter().all(|file| file.exists()), "{young:?}");

    let linked = Host::new();
    let runtime = folder(linked.runtime.path().join("luneta"));
    let elsewhere = tempfile::tempdir().unwrap();
    symlink(elsewhere.path(), runtime.join("images")).unwrap();
    let outside = file(elsewhere.path().join("past.png"), day);
    let past = file(folder(runtime.join("printed")).join("past.txt"), day);
    let _daemon = start(linked.vars()).unwrap();
    // The images are looked at first.
    eventually("the file past its day to go", || (!past.exists()).then_some(()));
    assert!(outside.exists(), "a file through a link stays");
}

/// Where a key goes for `pasted` on the machine `on`, as `daemon` hands on
/// the one answer its stream holds, by places of the panes `pasted` says; or
/// why there is none.
async fn pasted_on(
    daemon: &mut DaemonClient<Channel>,
    on: &str,
    pasted: &Pasted,
) -> Result<Ready<usize>, Status> {
    let request = pasted.asked(Some(On { destination: on.to_owned() }));
    let mut answer = daemon.paste(request).await?.into_inner();
    let ready = answer.message().await?.expect("an answer");
    assert_eq!(answer.message().await?, None, "the stream ends after the answer");
    Ok(pasted.answered(ready).expect("an answer luneta reads"))
}

/// A clone wt made in `scratch`, of `up` there, for a key of the menu on `host`:
/// zoxide there knows it, and gh there knows the issue 7 of `o/n`. That is the
/// repository on GitHub of a remote of the clone, which git takes for `up`.
fn clone_with_issue_7(host: &Host, scratch: &Path) -> PathBuf {
    host.gh.issue(7, "https://github.com/o/n/issues/7");
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(scratch, &up, &clone);
    let bare = clone.join(".bare");
    let github = "git@github.com:o/n.git";
    git(&bare, &["remote", "add", "upstream", github]);
    git(&bare, &["config", &format!("url.{}.insteadOf", up.display()), github]);
    let main = clone.join("main").display().to_string();
    script(&host.zellij.bin().join("zoxide"), &format!("echo '{main}'\n"));
    clone
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
    let scratch = tempfile::tempdir().unwrap();
    // As git says the worktree, by its real path, as on macOS, where the
    // temporary folder is through a link.
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let main = clone.join("main").display().to_string();
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let link = Work::Link(issue.clone());
        let worked = |dir: &str| links::Worked { dir: dir.to_owned(), pull: None, here: false };
        let session = Pasted::new(link.clone(), vec![worked("/nowhere/at/all"), worked(&main)]);
        let ready = pasted_on(&mut daemon, "far", &session).await.unwrap();
        let worktree = clone.join("issue-7").display().to_string();
        let branch = Some("issue-7".to_owned());
        let session_at = At::Session(1);
        let work = Readied::Link(issue);
        let answer = Ready {
            at: session_at,
            worktree: worktree.clone(),
            branch,
            moved_off: false,
            work,
        };
        assert_eq!(ready, answer);
        assert!(clone.join("issue-7").join(".git").is_file());
        let viewed = "issue view 7 --repo github.com/o/n --json url --jq .url";
        assert_eq!(far.gh.issues(), [viewed]);
        assert_eq!(here.gh.issues(), [] as [String; 0]);

        let made = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), Vec::new())).await;
        let Ready { at: At::Clone { cwd, name }, worktree: again, .. } = made.unwrap() else {
            panic!("no session to make");
        };
        assert_eq!((cwd, name.as_str(), again), (clone.display().to_string(), "n", worktree));

        let relative = Pasted::new(link.clone(), vec![worked("n/main")]);
        let relative = pasted_on(&mut daemon, "far", &relative).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");
        let nowhere = pasted_on(&mut daemon, "nowhere", &session).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::FailedPrecondition, "{nowhere:?}");
        assert_eq!(far.gh.issues().len(), 2);
    });
}

/// A machine with no session in a link's repository, and whose zoxide knows no
/// clone of it, says so apart from whatever else goes wrong, through the daemon
/// that bridges to it, for luneta to ask its other machines. It asks nothing of
/// GitHub for that, as its gh may be logged out. One with a clone git made says
/// what stands in the way instead, as does one with a directory git would not
/// say the remotes of, which may be a clone of it. One that has the repository
/// goes no further on an issue's number that is a pull request's on GitHub,
/// whether it has a session there or a pane on its branch.
#[test]
fn a_machine_with_none_of_a_link_s_repository_says_so_apart() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    script(&far.zellij.bin().join("zoxide"), "exit 0\n");
    far.gh.issue(8, "https://github.com/o/n/pull/8");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let github = "git@github.com:o/n.git";
    let (by_git, unread, by_wt) =
        (scratch.join("by-git"), scratch.join("unread"), scratch.join("by-wt"));
    git(&scratch, &["init", "-q", &by_git.display().to_string()]);
    git(&by_git, &["remote", "add", "origin", github]);
    git(&scratch, &["init", "-q", "--bare", &unread.display().to_string()]);
    git(&unread, &["remote", "add", "origin", github]);
    let config = unread.join("config");
    fs::write(&config, fs::read_to_string(&config).unwrap() + "[broken\n").unwrap();
    cloned_as_wt(&scratch, &scratch.join("up"), &by_wt);
    git(&by_wt.join(".bare"), &["remote", "add", "upstream", github]);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let link = Work::Link(Link::parse("https://github.com/o/n/pull/7").unwrap());
        let none = Pasted::new(link.clone(), Vec::new());
        let none = pasted_on(&mut daemon, "far", &none).await.unwrap_err();
        assert_eq!(none.code(), Code::NotFound, "{none:?}");
        assert!(none.message().starts_with("no session works in o/n"), "{none:?}");

        let issue = |number: u32| {
            Work::Link(Link::parse(&format!("https://github.com/o/n/issues/{number}")).unwrap())
        };
        let none = Pasted::new(issue(7), Vec::new());
        let none = pasted_on(&mut daemon, "far", &none).await.unwrap_err();
        assert_eq!(none.code(), Code::NotFound, "{none:?}");
        assert!(far.gh.issues().is_empty(), "{:?}", far.gh.issues());

        let in_dir = |dir: &Path| links::Worked {
            dir: dir.display().to_string(),
            pull: None,
            here: false,
        };
        let by_git = Pasted::new(link.clone(), vec![in_dir(&by_git)]);
        let by_git = pasted_on(&mut daemon, "far", &by_git).await.unwrap_err();
        assert_eq!(by_git.code(), Code::FailedPrecondition, "{by_git:?}");
        assert!(by_git.message().contains("that git made"), "{by_git:?}");

        let unsaid = Pasted::new(link, vec![in_dir(&unread)]);
        let unsaid = pasted_on(&mut daemon, "far", &unsaid).await.unwrap_err();
        assert_eq!(unsaid.code(), Code::FailedPrecondition, "{unsaid:?}");
        let whether = format!("git did not say whether {} is of o/n: ", unread.display());
        assert!(unsaid.message().starts_with(&whether), "{unsaid:?}");

        let pull = Link::parse("https://github.com/o/n/pull/8").unwrap();
        let on_it = links::Worked { pull: Some(pull), ..in_dir(Path::new("/nowhere/at/all")) };
        for panes in [vec![in_dir(&by_wt.join("main"))], vec![on_it]] {
            let kind = pasted_on(&mut daemon, "far", &Pasted::new(issue(8), panes)).await;
            let kind = kind.unwrap_err();
            assert_eq!(kind.code(), Code::FailedPrecondition, "{kind:?}");
            let paste = "it is a pull request on GitHub: paste https://github.com/o/n/pull/8";
            assert_eq!(kind.message(), paste, "{kind:?}");
        }
        assert_eq!(far.gh.issues().len(), 2, "{:?}", far.gh.issues());
    });
}

/// A remote by ssh is of a repository on GitHub under a name ssh's config gives
/// GitHub, as `ssh -G` says, which reads the config and connects nowhere. What
/// starts with `-` names no host, as ssh would take it for an option, and ssh is
/// not asked of it.
#[test]
fn ssh_says_what_host_a_remote_names_and_is_asked_of_no_option() {
    let host = Host::new();
    let runs = host.zellij.bin().join("ssh-runs");
    let ssh = format!(
        "echo \"$*\" >> {}\nif [ \"$2\" = github-work ]; then echo 'hostname github.com'; else echo \"hostname $2\"; fi\n",
        quoted(&runs.display().to_string())
    );
    script(&host.zellij.bin().join("ssh"), &ssh);
    script(&host.zellij.bin().join("zoxide"), "exit 0\n");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = |name: &str, url: &str| {
        let dir = scratch.join(name);
        git(&scratch, &["init", "-q", &dir.display().to_string()]);
        git(&dir, &["remote", "add", "origin", url]);
        dir
    };
    let option = clone("option", "git@-oProxyCommand:o/n.git");
    let renamed = clone("renamed", "git@github-work:o/n.git");
    let _daemon = start(host.vars()).unwrap();

    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let link = Work::Link(Link::parse("https://github.com/o/n/pull/7").unwrap());
        let answers = [
            (&option, Code::NotFound, "no session works in o/n"),
            (&renamed, Code::FailedPrecondition, "that git made"),
        ];
        for (dir, code, says) in answers {
            let pane = links::Worked { dir: dir.display().to_string(), pull: None, here: false };
            let asked = Pasted::new(link.clone(), vec![pane]).asked(None);
            let mut answer = daemon.paste(asked).await.unwrap().into_inner();
            let why = answer.message().await.unwrap_err();
            assert_eq!(why.code(), code, "{why:?}");
            assert!(why.message().contains(says), "{why:?}");
        }
    });
    let asked = fs::read_to_string(&runs).unwrap();
    assert!(!asked.contains("-oProxyCommand"), "{asked}");
}

fn git_said(dir: &Path, args: &[&str]) -> String {
    let mut git = Command::new("git");
    lunetad::local_git(&mut git);
    let said = git
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(said.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&said.stderr));
    String::from_utf8(said.stdout).unwrap().trim().to_owned()
}

/// A clone wt made, whose origin is spelled as GitHub's, as wt reads it, and
/// reached through an ssh of its own: `up` is `o/n`, and `fork` is `f/n`. Of
/// `feat`, the branch of `o/n#7`, it has only `origin/feat`.
fn clone_with_pull_7(host: &Host, scratch: &Path) -> PathBuf {
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(scratch, &up, &clone);
    pushed(host, &up, "feat", "first");
    let bare = clone.join(".bare");
    git(&bare, &["branch", "-q", "-D", "feat"]);
    git(&bare, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&bare, &["fetch", "-q", "origin"]);
    git(&bare, &["remote", "set-url", "origin", "git@github.com:o/n.git"]);
    let ssh = scratch.join("ssh-to-github");
    let text = format!(
        "case \"$*\" in\n    *\"'f/n.git'\"*) exec git upload-pack \"{0}/fork\" ;;\n    *) exec git \
         upload-pack \"{0}/up\" ;;\nesac\n",
        scratch.display()
    );
    script(&ssh, &text);
    git(&bare, &["config", "core.sshCommand", &ssh.display().to_string()]);
    git(&bare, &["config", "ssh.variant", "simple"]);
    clone
}

fn pushed(host: &Host, up: &Path, parent: &str, name: &str) -> String {
    let head = git_said(up, &["commit-tree", "-p", parent, "-m", name, "feat^{tree}"]);
    git(up, &["update-ref", "refs/heads/feat", &head]);
    git(up, &["update-ref", "refs/pull/7/head", &head]);
    host.gh.pull(7, "main", &head);
    head
}

fn reviewing_pull_7(worked: Vec<links::Worked>) -> Pasted {
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    Pasted::new(Work::Link(link), worked).wanting(Wanted::AtHead)
}

fn pasted_here(host: &Host, pasted: &Pasted) -> Result<Ready<usize>, Status> {
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let mut answer = daemon.paste(pasted.asked(None)).await?.into_inner();
        let ready = answer.message().await?.expect("an answer");
        Ok(pasted.answered(ready).expect("an answer luneta reads"))
    })
}

fn pane_in(dir: &Path) -> links::Worked {
    links::Worked { dir: dir.display().to_string(), pull: None, here: false }
}

#[test]
fn r_brings_the_worktree_wt_found_to_the_pull_request_s_head() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, bare, main) = (scratch.join("up"), clone.join(".bare"), clone.join("main"));
    let feat = clone.join("feat");
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&main)]);
    let at_head = |head: &str, how: &str| {
        let ready = pasted_here(&host, &pasted).unwrap();
        assert_eq!(ready.at, At::Session(0), "{how}");
        assert_eq!(ready.worktree, feat.display().to_string(), "{how}");
        assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head, "{how}");
    };

    let local_main = git_said(&main, &["rev-parse", "main"]);
    git(&up, &["commit", "-q", "--allow-empty", "-m", "on main"]);
    at_head(&pushed(&host, &up, "feat", "second"), "known only as last fetched");
    let tracked_main = git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]);
    assert_eq!(tracked_main, git_said(&up, &["rev-parse", "main"]));
    assert_eq!(git_said(&main, &["rev-parse", "main"]), local_main);
    assert_eq!(host.gh.viewed(), ["pr view 7 --repo github.com/o/n --json baseRefName,headRefOid"]);

    at_head(&pushed(&host, &up, "feat", "third"), "a worktree there already");
    at_head(&pushed(&host, &up, "main", "rewritten"), "rewritten");
    at_head(&pushed(&host, &up, "main", "rewritten again"), "rewritten again");
    git(&bare, &["worktree", "remove", "../feat"]);
    at_head(&pushed(&host, &up, "feat", "fourth"), "a local branch whose worktree went");
}

#[test]
fn w_leaves_the_worktree_where_it_is() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let up = scratch.join("up");
    let feat = clone.join("feat");
    let _daemon = start(host.vars()).unwrap();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pasted = Pasted::new(Work::Link(link), vec![pane_in(&clone.join("main"))]);

    pasted_here(&host, &pasted).unwrap();
    let was = git_said(&feat, &["rev-parse", "HEAD"]);
    let tracked_main = git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]);
    git(&up, &["commit", "-q", "--allow-empty", "-m", "on main"]);
    pushed(&host, &up, "feat", "second");
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), was);
    assert_eq!(git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]), tracked_main);
    assert_eq!(host.gh.viewed(), [] as [String; 0]);
}

/// A pane on the branch runs no wt, and syncs all the same. `Code::Aborted` is
/// what has no other machine make a second worktree.
#[test]
fn r_on_a_far_machine_s_pane_syncs_there_or_says_why_it_did_not() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&far, &scratch);
    let (up, feat) = (scratch.join("up"), clone.join("feat"));
    git(&clone.join(".bare"), &["worktree", "add", "-q", "--track", "-b", "feat", "../feat", "origin/feat"]);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pane = links::Worked { pull: Some(link), ..pane_in(&feat) };
    let pasted = reviewing_pull_7(vec![pane]);
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let head = pushed(&far, &up, "feat", "second");
        let ready = pasted_on(&mut daemon, "far", &pasted).await.unwrap();
        assert_eq!(ready.at, At::There(0));
        assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);

        pushed(&far, &up, "feat", "third");
        fs::write(feat.join("mine"), "mine").unwrap();
        git(&feat, &["add", "mine"]);
        let refused = pasted_on(&mut daemon, "far", &pasted).await.unwrap_err();
        assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
        let why = format!(
            "started no review in {}: the worktree has changes that are not committed",
            feat.display()
        );
        assert_eq!(refused.message(), why);
        assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    });
    assert_eq!(here.gh.viewed(), [] as [String; 0]);
}

/// Nothing has to move, so nothing the worktree holds is in danger, and no wt
/// has to say what it follows: at the head is the code to review.
#[test]
fn r_at_the_head_moves_nothing_whatever_is_checked_out() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let feat = clone.join("feat");
    git(&clone.join(".bare"), &["worktree", "add", "-q", "--track", "-b", "feat", "../feat", "origin/feat"]);
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&clone.join("main"))]);
    let head = git_said(&feat, &["rev-parse", "HEAD"]);

    fs::write(feat.join("mine"), "mine").unwrap();
    git(&feat, &["add", "mine"]);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(feat.join("mine")).unwrap(), "mine");

    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pane = links::Worked { pull: Some(link), ..pane_in(&feat) };
    git(&feat, &["switch", "-q", "--detach"]);
    let ready = pasted_here(&host, &reviewing_pull_7(vec![pane])).unwrap();
    assert_eq!(ready.at, At::There(0));
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
}

/// The sidebar's "this pane has #7" can lag a `git switch main`. wt follows what
/// is checked out, which is not the pull request.
#[test]
fn r_on_a_pane_gone_to_another_branch_starts_no_review() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let main = clone.join("main");
    git(&main, &["branch", "-q", "--set-upstream-to=origin/main", "main"]);
    let _daemon = start(host.vars()).unwrap();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pane = links::Worked { pull: Some(link), ..pane_in(&main) };

    let refused = pasted_here(&host, &reviewing_pull_7(vec![pane])).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    assert!(refused.message().contains("and the head of o/n#7 is"), "{refused:?}");
    let up_main = git_said(&scratch.join("up"), &["rev-parse", "main"]);
    assert_eq!(git_said(&main, &["rev-parse", "HEAD"]), up_main);
}

#[test]
fn r_from_a_directory_the_head_leaves_out_says_so() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, work, feat) = (scratch.join("up"), scratch.join("work"), clone.join("feat"));
    git(&up, &["worktree", "add", "-q", "--detach", &work.display().to_string(), "feat"]);
    let pushed_from_work = || {
        let head = git_said(&work, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/heads/feat", &head]);
        git(&up, &["update-ref", "refs/pull/7/head", &head]);
        host.gh.pull(7, "main", &head);
        head
    };
    fs::create_dir(work.join("sub")).unwrap();
    fs::write(work.join("sub/file"), "file").unwrap();
    git(&work, &["add", "sub"]);
    git(&work, &["commit", "-q", "-m", "sub"]);
    pushed_from_work();
    let _daemon = start(host.vars()).unwrap();
    pasted_here(&host, &reviewing_pull_7(vec![pane_in(&clone.join("main"))])).unwrap();
    assert!(feat.join("sub/file").is_file());

    git(&work, &["rm", "-rq", "sub"]);
    git(&work, &["commit", "-q", "-m", "no sub"]);
    let head = pushed_from_work();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pane = links::Worked { pull: Some(link), ..pane_in(&feat.join("sub")) };
    let refused = pasted_here(&host, &reviewing_pull_7(vec![pane])).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    let why = "the worktree moved, and where it went has no such directory";
    assert!(refused.message().ends_with(why), "{refused:?}");
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_said(&feat, &["rev-parse", "refs/remotes/origin/feat"]), head);
}

#[test]
fn r_refused_on_a_rewrite_follows_it_once_the_change_is_gone() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, feat) = (scratch.join("up"), clone.join("feat"));
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&clone.join("main"))]);
    pasted_here(&host, &pasted).unwrap();
    let was = git_said(&feat, &["rev-parse", "HEAD"]);

    let head = pushed(&host, &up, "main", "rewritten");
    fs::write(feat.join("mine"), "mine").unwrap();
    git(&feat, &["add", "mine"]);
    let refused = pasted_here(&host, &pasted).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    assert!(refused.message().contains("changes that are not committed"), "{refused:?}");
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), was);

    git(&feat, &["rm", "-qf", "mine"]);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
}

#[test]
fn r_on_a_rewrite_another_fetch_saw_first_says_what_moves_the_branch() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, feat) = (scratch.join("up"), clone.join("feat"));
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&clone.join("main"))]);
    pasted_here(&host, &pasted).unwrap();
    let was = git_said(&feat, &["rev-parse", "HEAD"]);

    let head = pushed(&host, &up, "main", "rewritten");
    git(&clone.join(".bare"), &["fetch", "-q", "origin"]);
    let refused = pasted_here(&host, &pasted).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    let command = format!("run git switch --no-overwrite-ignore -C feat {head}");
    assert!(refused.message().ends_with(&command), "{refused:?}");
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), was);
}

#[test]
fn r_goes_by_the_head_alone_where_no_remote_names_the_pull_request_s_repository() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, bare, feat) = (scratch.join("up"), clone.join(".bare"), clone.join("feat"));
    git(&bare, &["worktree", "add", "-q", "--track", "-b", "feat", "../feat", "origin/feat"]);
    git(&bare, &["remote", "set-url", "origin", "git@github.com:old/n.git"]);
    let _daemon = start(host.vars()).unwrap();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let pane = links::Worked { pull: Some(link), ..pane_in(&feat) };
    let pasted = reviewing_pull_7(vec![pane]);
    let tracked_main = git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]);

    assert_eq!(pasted_here(&host, &pasted).unwrap().at, At::There(0));

    git(&up, &["commit", "-q", "--allow-empty", "-m", "on main"]);
    let head = pushed(&host, &up, "feat", "second");
    assert_eq!(pasted_here(&host, &pasted).unwrap().at, At::There(0));
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]), tracked_main);

    pushed(&host, &up, "feat", "third");
    fs::write(feat.join("mine"), "mine").unwrap();
    git(&feat, &["add", "mine"]);
    let refused = pasted_here(&host, &pasted).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    assert!(refused.message().contains("changes that are not committed"), "{refused:?}");
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);

    git(&bare, &["config", "core.repositoryformatversion", "99"]);
    let refused = pasted_here(&host, &pasted).unwrap_err();
    assert_eq!(refused.code(), Code::Aborted, "{refused:?}");
    assert!(refused.message().contains("git did not say the remotes of"), "{refused:?}");
}

#[test]
fn r_runs_nothing_a_pull_request_brings() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, work, feat) = (scratch.join("up"), scratch.join("work"), clone.join("feat"));
    git(&clone.join(".bare"), &["config", "core.hooksPath", ".githooks"]);
    git(&clone.join(".bare"), &["config", "core.fsmonitor", ".githooks/fsmonitor"]);
    git(&up, &["worktree", "add", "-q", "--detach", &work.display().to_string(), "feat"]);
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&clone.join("main"))]);
    pasted_here(&host, &pasted).unwrap();

    let ran = scratch.join("hook-ran");
    fs::create_dir(work.join(".githooks")).unwrap();
    for hook in ["post-checkout", "reference-transaction", "fsmonitor"] {
        let text = format!("echo {hook} >> \"{}\"\n", ran.display());
        script(&work.join(".githooks").join(hook), &text);
    }
    git(&work, &["add", ".githooks"]);
    git(&work, &["commit", "-q", "-m", "hooks"]);
    let head = git_said(&work, &["rev-parse", "HEAD"]);
    git(&up, &["update-ref", "refs/heads/feat", &head]);
    git(&up, &["update-ref", "refs/pull/7/head", &head]);
    host.gh.pull(7, "main", &head);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert!(!ran.exists(), "the move ran {:?}", fs::read_to_string(&ran));

    let main = git_said(&up, &["commit-tree", "-p", "main", "-m", "base moves", "main^{tree}"]);
    git(&up, &["update-ref", "refs/heads/main", &main]);
    let head = pushed(&host, &up, "feat", "one more");
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_said(&feat, &["rev-parse", "refs/remotes/origin/main"]), main);
    assert!(!ran.exists(), "the base's fetch ran {:?}", fs::read_to_string(&ran));
}

#[test]
fn r_keeps_the_git_config_of_the_daemon_s_environment() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, bare) = (scratch.join("up"), clone.join(".bare"));
    let ssh = git_said(&bare, &["config", "core.sshCommand"]);
    git(&bare, &["config", "--unset", "core.sshCommand"]);
    let mut lunetad = host.vars().command(LUNETAD);
    lunetad
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.sshCommand")
        .env("GIT_CONFIG_VALUE_0", &ssh);
    let _daemon = start_as(lunetad).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&clone.join("main"))]);
    pasted_here(&host, &pasted).unwrap();

    let head = pushed(&host, &up, "feat", "one more");
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&clone.join("feat"), &["rev-parse", "HEAD"]), head);
}

/// wt names the branch `f-main`, as the clone has a `main` of its own.
#[test]
fn r_syncs_a_pull_request_from_a_fork_s_main() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let (up, fork, main) = (scratch.join("up"), scratch.join("fork"), clone.join("main"));
    git(&scratch, &["clone", "-q", "up", "fork"]);
    let opened = |name: &str, amend: bool| {
        let amended: &[&str] = if amend { &["--amend"] } else { &[] };
        git(&fork, &[&["commit", "-q", "--allow-empty", "-m", name], amended].concat());
        let spec = "+refs/heads/main:refs/pull/7/head";
        git(&up, &["fetch", "-q", &fork.display().to_string(), spec]);
        let head = git_said(&fork, &["rev-parse", "main"]);
        host.gh.pull(7, "main", &head);
        head
    };
    host.gh.api("f\thttps://github.com/f/n.git\tgit@github.com:f/n.git\tmain\n");
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![pane_in(&main)]);
    let local_main = git_said(&main, &["rev-parse", "main"]);
    let worktree = clone.join("f-main");

    let head = opened("from the fork", false);
    let ready = pasted_here(&host, &pasted).unwrap();
    assert_eq!(ready.worktree, worktree.display().to_string());
    assert_eq!(ready.branch.as_deref(), Some("f-main"));
    assert_eq!(git_said(&worktree, &["rev-parse", "HEAD"]), head);

    let head = opened("rewritten on the fork", true);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_said(&worktree, &["rev-parse", "refs/remotes/f/main"]), head);
    assert_eq!(git_said(&main, &["rev-parse", "main"]), local_main);
}

/// A machine that made the worktree, and could not then say where the key goes,
/// says so apart from a machine that made nothing: it is the one failure that
/// leaves something behind, and no other machine should make another.
#[test]
fn a_machine_that_made_the_worktree_and_went_no_further_says_so_apart() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&host, &scratch);
    let _daemon = start(host.vars()).unwrap();
    host.hide_kept_sessions();

    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let asked = Pasted::new(Work::Link(issue), Vec::new()).asked(None);
        let mut answer = daemon.paste(asked).await.unwrap().into_inner();
        let why = answer.message().await.unwrap_err();
        assert_eq!(why.code(), Code::Aborted, "{why:?}");
    });
    assert!(clone.join("issue-7").join(".git").is_file());
}

/// A machine whose answer stalls on its way back started on the key as it heard
/// it, and makes the worktree with no one left to hear of it. So what luneta
/// hears by then says it may have, for no other machine to make a second, though
/// it comes before the answer's stream does.
#[test]
fn a_machine_whose_answer_stalls_may_have_made_the_worktree() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    ssh.answers_through_a_cat("far");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _stalled = ssh.stalls_answers("far");
    let said = runtime.block_on(async {
        let daemon = lunetad::connect(&folder).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let pasted = Pasted::new(Work::Link(issue), Vec::new());
        pasted.ready_on(daemon, &"far".parse().unwrap(), Duration::from_secs(6)).await
    });
    assert!(matches!(said, Err(Unready::InDoubt(_))), "{said:?}");
    let made = clone.join("issue-7").join(".git");
    eventually("the worktree on far", || made.is_file().then_some(()));
}

/// A `.md` file pasted on a row of another machine is read there, and git
/// there says the repository it is in, untracked in a worktree or not, whose
/// session the key goes to, by what the pane's directory is in, whatever its
/// remotes are; gh is not asked. The worktree is of a branch named for it,
/// after `md-`, from origin's default branch as it is now, not as the clone
/// last fetched it. Pasted again, it finds that worktree, and a pane that works
/// in it; and a file named as a branch is finds no worktree of that. A file in
/// no repository goes by the row's, and a relative path from it, and `~/` is
/// the home of the machine there, not of this one. A path that names no file,
/// or one in a clone git made, or in a repository git refuses to read, makes
/// nothing, and says why.
#[test]
fn a_file_pasted_on_another_machine_s_row_is_read_and_made_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let main = clone.join("main").display().to_string();
    let plans = clone.join("main").join("docs/plans");
    fs::create_dir_all(&plans).unwrap();
    let plan = plans.join("Sync Rework.md");
    fs::write(&plan, "written down").unwrap();
    let plan = plan.display().to_string();
    // A commit origin has that the clone has not fetched.
    git(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let tip = rev_parse(&up, "main");
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let file = |path: &str, from: Option<&str>| {
            let from = from.map(|from| Directory::parse(from.to_owned()).unwrap());
            Work::File(links::File::parse(path, from).unwrap())
        };
        let worked = |dir: &str| links::Worked { dir: dir.to_owned(), pull: None, here: false };
        let panes = vec![worked("/nowhere/at/all"), worked(&main)];
        let asked = Pasted::new(file(&plan, Some("/nowhere/at/all")), panes.clone());
        let ready = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let worktree = clone.join("md-sync-rework").display().to_string();
        let branch = Some("md-sync-rework".to_owned());
        let of_plan = || Readied::File { real: plan.clone() };
        let answer = Ready {
            at: At::Session(1),
            worktree: worktree.clone(),
            branch: branch.clone(),
            moved_off: false,
            work: of_plan(),
        };
        assert_eq!(ready, answer);
        assert!(clone.join("md-sync-rework").join(".git").is_file());
        assert_eq!(rev_parse(&clone.join("md-sync-rework"), "HEAD"), tip);
        assert_eq!(far.gh.issues(), [] as [String; 0]);

        let mut in_it = panes.clone();
        in_it.push(worked(&worktree));
        let asked = Pasted::new(file(&plan, None), in_it);
        let again = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(again.at, At::There(2));
        assert_eq!(again.worktree, worktree);

        // No pane in the repository: a session to make in what holds it.
        let asked = Pasted::new(file(&plan, None), vec![worked("/nowhere/at/all")]);
        let made = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let At::Clone { cwd, name } = made.at else { panic!("no session to make") };
        assert_eq!((cwd, name.as_str()), (clone.display().to_string(), "n"));

        // In no repository, by the far machine's `~/`: the row's repository,
        // and the file where it is.
        let home = far.home.path().canonicalize().unwrap();
        fs::write(home.join("errand.md"), "written down").unwrap();
        let asked = Pasted::new(file("~/errand.md", Some(&main)), panes.clone());
        let errand = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(errand.at, At::Session(1));
        assert_eq!(errand.worktree, clone.join("md-errand").display().to_string());
        let real = home.join("errand.md").display().to_string();
        assert_eq!(errand.work, Readied::File { real });

        // From the row, relative.
        let asked = Pasted::new(file("docs/plans/Sync Rework.md", Some(&main)), panes.clone());
        let relative = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(relative.work, of_plan());
        assert_eq!(relative.worktree, worktree);

        // Named as a branch is: a branch of its own, and none of that one's.
        let named = plans.join("main.md");
        fs::write(&named, "written down").unwrap();
        let asked = Pasted::new(file(&named.display().to_string(), None), panes.clone());
        let of_main = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(of_main.worktree, clone.join("md-main").display().to_string());
        assert_eq!(of_main.branch.as_deref(), Some("md-main"));

        // A file this machine has, and the far one has not.
        let mine = here.home.path().canonicalize().unwrap().join("mine.md");
        fs::write(&mine, "written down").unwrap();
        let asked = Pasted::new(file("~/mine.md", Some(&main)), panes.clone());
        assert_eq!(refused_there(&mut daemon, &asked).await, "no such file");
        assert!(!clone.join("md-mine").exists());

        let outside = scratch.join("outside.md");
        fs::write(&outside, "written down").unwrap();
        let asked = Pasted::new(file(&outside.display().to_string(), None), panes.clone());
        assert_eq!(
            refused_there(&mut daemon, &asked).await,
            "it is in no repository, and the row highlighted works in no directory to go by"
        );

        let by_git = scratch.join("by-git");
        git(&scratch, &["clone", "-q", &up.display().to_string(), &by_git.display().to_string()]);
        fs::write(by_git.join("plan.md"), "written down").unwrap();
        let in_by_git = by_git.join("plan.md").display().to_string();
        let asked = Pasted::new(file(&in_by_git, None), panes.clone());
        assert_eq!(
            refused_there(&mut daemon, &asked).await,
            format!(
                "{} is in a clone git made, and wt would make the worktree in its .git; wt clone \
                 it first",
                by_git.display()
            )
        );
        assert_eq!(rev_parse_verify(&by_git, "refs/heads/md-plan"), None);

        // In a repository git refuses to read, on a row of one it reads: no
        // word that the file is in no repository, so nothing of the row's.
        let unread = scratch.join("unread");
        git(&scratch, &["init", "-q", &unread.display().to_string()]);
        git(&unread, &["config", "core.repositoryformatversion", "99"]);
        fs::write(unread.join("in-it.md"), "written down").unwrap();
        let in_it = unread.join("in-it.md").display().to_string();
        let asked = Pasted::new(file(&in_it, Some(&main)), panes);
        let why = refused_there(&mut daemon, &asked).await;
        assert!(why.starts_with("git did not say what repository it is in: fatal: "), "{why}");
        assert!(!clone.join("md-in-it").exists());
        assert_eq!(rev_parse_verify(&clone.join("main"), "refs/heads/md-in-it"), None);
    });
}

#[test]
fn a_worktree_where_an_issue_s_would_go_is_said_to_have_moved_off_its_branch() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    far.gh.issue(8, "https://github.com/o/n/issues/8");
    far.gh.issue(9, "https://github.com/o/n/issues/9");
    far.gh.issue(10, "https://github.com/o/n/issues/10");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let (bare, main, moved) = (clone.join(".bare"), clone.join("main"), clone.join("issue-7"));
    git(&bare, &["worktree", "add", "-q", "../issue-7", "feat"]);
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| !far.sessions.is_empty());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let link = Work::Link(issue.clone());
        let panes = vec![pane_in(&main)];
        let asked = Pasted::new(link.clone(), panes.clone());
        let worktree = moved.display().to_string();

        let opened = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let (at, work) = (At::Session(0), Readied::Link(issue));
        let branch = Some("feat".to_owned());
        let answer = Ready { at, worktree: worktree.clone(), branch, moved_off: true, work };
        assert_eq!(opened, answer);
        assert_eq!(rev_parse_verify(&main, "refs/heads/issue-7"), None);

        let in_it = vec![pane_in(&main), pane_in(&moved)];
        let there = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), in_it)).await;
        assert_eq!(there.unwrap().at, At::There(1));

        let none = Pasted::new(link.clone(), Vec::new());
        let made = pasted_on(&mut daemon, "far", &none).await.unwrap();
        assert!(matches!(made.at, At::Clone { .. }), "{made:?}");
        assert_eq!(made.worktree, worktree);

        git(&moved, &["switch", "-q", "--detach"]);
        let detached = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!((detached.worktree, detached.branch), (worktree.clone(), None));
        assert!(detached.moved_off);

        git(&moved, &["switch", "-q", "-c", "issue-7"]);
        let on_it = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(on_it.worktree, worktree);
        assert_eq!(on_it.branch.as_deref(), Some("issue-7"));
        assert!(!on_it.moved_off);

        git(&moved, &["switch", "-q", "--detach"]);
        git(&bare, &["worktree", "add", "-q", "../beside", "issue-7"]);
        let beside = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(beside.worktree, clone.join("beside").display().to_string());
        assert!(!beside.moved_off);

        fs::create_dir(clone.join("issue-8")).unwrap();
        let eight = Work::Link(Link::parse("https://github.com/o/n/issues/8").unwrap());
        let why = refused_there(&mut daemon, &Pasted::new(eight, panes.clone())).await;
        assert!(why.ends_with("issue-8 is already present"), "{why}");

        git(&bare, &["worktree", "add", "-q", "-b", "gone", "../issue-10", "main"]);
        fs::remove_dir_all(clone.join("issue-10")).unwrap();
        fs::create_dir(clone.join("issue-10")).unwrap();
        let ten = Work::Link(Link::parse("https://github.com/o/n/issues/10").unwrap());
        let why = refused_there(&mut daemon, &Pasted::new(ten, panes.clone())).await;
        assert!(why.ends_with("issue-10 is already present"), "{why}");

        let plan = main.join("plan.md");
        fs::write(&plan, "written down").unwrap();
        let plan = plan.display().to_string();
        git(&bare, &["worktree", "add", "-q", "-b", "other", "../md-plan", "main"]);
        let moved = clone.join("md-plan").display().to_string();
        let file = Work::File(links::File::parse(&plan, None).unwrap());
        let asked = Pasted::new(file, panes.clone());
        let opened = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let (at, work) = (At::Session(0), Readied::File { real: plan.clone() });
        let branch = Some("other".to_owned());
        assert_eq!(opened, Ready { at, worktree: moved, branch, moved_off: true, work });
        assert_eq!(rev_parse_verify(&main, "refs/heads/md-plan"), None);

        git(&bare, &["worktree", "add", "-q", "-b", "nine", "../issue-9", "main"]);
        let other = Work::Link(Link::parse("https://github.com/o/n/issues/9").unwrap());
        let other = Pasted::new(other, panes);
        assert!(pasted_on(&mut daemon, "far", &other).await.unwrap().moved_off);
        let on = Some(On { destination: "far".to_owned() });
        let older = PasteRequest { reads_moved_off: false, ..other.asked(on) };
        let mut answer = daemon.paste(older).await.unwrap().into_inner();
        let refused = answer.message().await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
        assert!(refused.message().ends_with("issue-9 is already present"), "{refused:?}");
    });
}

/// Why the daemon of the machine `far` found nothing for `pasted`.
async fn refused_there(daemon: &mut DaemonClient<Channel>, pasted: &Pasted) -> String {
    let refused = pasted_on(daemon, "far", pasted).await.unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    refused.message().to_owned()
}

/// What git says `what` is, in the repository `dir` is in.
fn rev_parse(dir: &Path, what: &str) -> String {
    rev_parse_verify(dir, what).expect("what git knows")
}

/// The same, or none where git knows none.
fn rev_parse_verify(dir: &Path, what: &str) -> Option<String> {
    let said = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "--quiet", what])
        .output()
        .unwrap();
    said.status.success().then(|| String::from_utf8(said.stdout).unwrap().trim().to_owned())
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
        assert_eq!(here.zellij.closed(), [] as [String; 0]);
        let idle = daemon.open(alone_in("idle", SystemTime::UNIX_EPOCH)).await.unwrap_err();
        assert_eq!(idle.code(), Code::FailedPrecondition, "{idle:?}");
        assert!(idle.message().contains("no client is in idle"), "{idle:?}");
        assert_eq!(far.zellij.closed(), closed);
    });
}
