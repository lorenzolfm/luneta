//! The other machines a daemon watches, as its hosts file names them, through
//! `ssh <destination> luneta bridge`: here, through an ssh of the test's own,
//! which runs the bridge of another host of the test's.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use lunetad::api::remote::Reached;
use lunetad::api::{self, WatchRequest};

use crate::demand::Detached;
use crate::host::{eventually, script, start, Host, Vars, LUNETA};

/// An `ssh` of a test's own, in a folder to put first on the `PATH` of a
/// host's daemon, that reaches the test's other hosts by the destinations it
/// was given for them with `reaches`: it runs `luneta bridge` with their
/// variables, as ssh runs it on the machine it reaches. Of a destination it
/// was given none for, it says it cannot resolve it, and ends as ssh does
/// then, with 255. Nor does it run anything on a terminal, which would mangle
/// what the daemons say, or where it could ask for a password, which no one
/// would answer: without `-T` and `BatchMode=yes`, it ends so too. It notes
/// the pid of each bridge it runs, for `bridges`.
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
fn reached(host: &Host, destination: &str, wanted: impl Fn(&api::Host) -> bool) -> api::Host {
    eventually(&format!("{destination} is reached as wanted"), || {
        let remote = host.host().remotes.into_iter().find(|it| it.destination == destination)?;
        match remote.reached? {
            Reached::Host(reached) if wanted(&reached) => Some(reached),
            _ => None,
        }
    })
}

/// The names of the sessions of `host`, in order.
fn names(host: &api::Host) -> Vec<&str> {
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
/// through ssh: one whose daemon answers, with that daemon's host, without
/// its version, as it changes, and one ssh cannot reach, with why.
/// `luneta sessions` lists the sessions of each under it.
#[test]
fn a_daemon_says_each_machine_its_hosts_file_names() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.zellij.bin());
    ssh.reaches("far", far.vars());
    let _work = far.serve("work");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "nowhere\nfar\n");
    let _here = start(here.vars()).unwrap();

    let first = reached(&here, "far", |far| !far.sessions.is_empty());
    assert_eq!(names(&first), ["work"]);
    assert_eq!(first.version, None);
    let _play = far.serve("play");
    reached(&here, "far", |far| names(far) == ["play", "work"]);

    let remotes = here.host().remotes;
    let order: Vec<&str> = remotes.iter().map(|it| it.destination.as_str()).collect();
    assert_eq!(order, ["nowhere", "far"]);
    match &remotes[0].reached {
        Some(Reached::Unreached(unreached)) => {
            assert!(unreached.why.contains("exit status: 255"), "{}", unreached.why);
        },
        other => panic!("nowhere is {other:?}"),
    }
    let output = here.vars().command(LUNETA).arg("sessions").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let said = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert!(lines[0].starts_with("nowhere/  (unreached for "), "{said}");
    let far = ["far/", "  play  (no panes listed yet)", "  work  (no panes listed yet)"];
    assert_eq!(lines[1..], far, "{said}");
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

/// Two daemons that watch each other come to rest: each says the other's
/// host without its remotes and its version, which would else change each
/// time the other's does, and the other's each time it does, for as long as
/// both ran.
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
    let far_said = reached(&here, "far", |_| true);
    reached(&far, "here", |_| true);
    assert!(far_said.remotes.is_empty());

    // The daemon answers with the host as it was once it has not changed
    // for 4 seconds.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = crate::host::folder_in(here.runtime.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let mut after = None;
        loop {
            let watched = daemon.watch(WatchRequest { after }).await.unwrap().into_inner();
            if watched.version.is_some() && watched.version == after {
                return;
            }
            assert!(Instant::now() < deadline, "the host never came to rest");
            after = watched.version;
        }
    });
}
