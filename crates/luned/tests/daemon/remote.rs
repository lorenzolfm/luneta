use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{symlink, DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use lune_protocol::said::Said;
use lune_waits::{eventually, Began, Span, QUIET};
use luned::api::remote::unreached::{self, Stopped};
use luned::api::remote::{self, Reached};
use luned::api::keep_image_request::{Image, Part};
use luned::api::keep_checks_response::{failed, Failed};
use luned::api::list_skills_response::Skill;
use luned::api::projects::Held;
use luned::api::{
    self, AskRequest, FileIssueRequest, IssueRepositoriesRequest, IssueRepository,
    KeepChecksRequest, KeepImageRequest, ListContentsRequest, ListRepositoriesRequest,
    ListSkillsRequest, On, PasteRequest, PrintedFile as File, TranscriptEndsRequest, WatchRequest,
};
use luned::asks::{Asked, Step, Turn};
use luned::images::{self, Png};
use luned::issues::Filed;
use luned::links::{self, At, Link, Pasted, Readied, Ready, Unready, Wanted, Work};
use luned::transcripts::SessionId;
use luned::{release, Daemon, Directory, ALONE, KEPT, SWEEP};
use rustix::process::{kill_process, test_kill_process, Pid, Signal};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Status};

use crate::demand::{bridged_stand_in, stop, Detached};
use crate::host::{
    answered, folder_in, script, shared_script, start, start_as, Host, Slot, Vars, LUNE, LUNED,
    PATIENT,
};
use crate::projects::project;
use crate::worktree::{cloned_as_wt, git};

const SLOW: Duration = QUIET.saturating_mul(6);

const GIVES_UP: Duration = QUIET.saturating_mul(3);

pub struct FakeSsh {
    to: PathBuf,
}

impl FakeSsh {
    pub fn new(bin: &Path) -> Self {
        let ssh = FakeSsh { to: bin.join("ssh-to") };
        fs::create_dir(&ssh.to).unwrap();
        let text = r#"to="${0%/*}/ssh-to"
quiet= batch=
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
echo run >> "$to/$destination.runs"
if [ ! -x "$to/$destination" ]; then
    echo unresolved >> "$to/$destination.unresolved"
    echo "ssh: Could not resolve hostname $destination: Name or service not known" >&2
    exit 255
fi
if [ "$1" = lune ] && [ "$2" = terminate ] && [ -x "$to/$destination.lune" ]; then
    shift
    exec "$to/$destination.lune" "$@"
fi
if [ "$*" != "lune bridge" ]; then
    echo "this ssh runs lune bridge alone, and was asked: $*" >&2
    exit 255
fi
echo $$ >> "$to/$destination.bridges"
exec "$to/$destination"
"#;
        shared_script(&bin.join("ssh"), text);
        ssh
    }

    pub fn reaches(&self, destination: &str, vars: Vars) {
        script(&self.to.join(destination), &bridge(&vars));
    }

    pub fn runs_lune(&self, destination: &str, vars: Vars) {
        script(&self.to.join(format!("{destination}.lune")), &lune(&vars, "\"$@\""));
    }

    fn reaches_a_stand_in(&self, destination: &str, version: &str) {
        script(&self.to.join(destination), &bridged_stand_in(version));
    }

    fn reaches_a_stand_in_slowly(&self, destination: &str, version: &str, secs: u32) {
        script(&self.to.join(destination), &format!("sleep {secs}\n{}", bridged_stand_in(version)));
    }

    fn reaches_slowly(&self, destination: &str, vars: Vars, secs: u32) {
        script(&self.to.join(destination), &format!("sleep {secs}\n{}", bridge(&vars)));
    }

    fn ends_with(&self, destination: &str, code: u8) {
        script(&self.to.join(destination), &format!("exit {code}\n"));
    }

    fn unreaches(&self, destination: &str) {
        fs::remove_file(self.to.join(destination)).unwrap();
    }

    pub fn runs(&self, destination: &str) -> usize {
        let runs = fs::read_to_string(self.to.join(format!("{destination}.runs")));
        runs.unwrap_or_default().lines().count()
    }

    fn unresolved(&self, destination: &str) -> usize {
        let unresolved = fs::read_to_string(self.to.join(format!("{destination}.unresolved")));
        unresolved.unwrap_or_default().lines().count()
    }

    fn bridges(&self, destination: &str) -> Vec<u32> {
        let path = self.to.join(format!("{destination}.bridges"));
        let bridges = fs::read_to_string(path).unwrap_or_default();
        bridges.lines().map(|pid| pid.parse().unwrap()).collect()
    }

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

    fn stalls_answers(&self, destination: &str) -> StalledAnswers {
        let cat = fs::read_to_string(self.to.join(format!("{destination}.cat"))).unwrap();
        let cat = Pid::from_raw(cat.trim().parse().unwrap()).unwrap();
        kill_process(cat, Signal::STOP).unwrap();
        StalledAnswers(cat)
    }
}

struct StalledAnswers(Pid);

impl Drop for StalledAnswers {
    fn drop(&mut self) {
        let _ = kill_process(self.0, Signal::CONT);
    }
}

fn bridge(vars: &Vars) -> String {
    lune(vars, "bridge")
}

fn lune(vars: &Vars, args: &str) -> String {
    let bridge = vars.command(&LUNE);
    let (mut unset, mut set) = (String::new(), String::new());
    for (name, value) in bridge.get_envs() {
        let name = name.to_str().unwrap();
        match value {
            None => write!(unset, " -u {name}").unwrap(),
            Some(value) => write!(set, " {name}={}", quoted(value.to_str().unwrap())).unwrap(),
        }
    }
    format!("exec env{unset}{set} {} {args}\n", quoted(&LUNE))
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn hosts(host: &Host, text: &str) {
    let dir = host.home.path().join(".local/state/lune");
    fs::create_dir_all(&dir).unwrap();
    let writing = dir.join("hosts.writing");
    fs::write(&writing, text).unwrap();
    fs::rename(writing, dir.join("hosts")).unwrap();
}

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

fn unreached(host: &Host, destination: &str) -> remote::Unreached {
    eventually(&format!("{destination} is unreached"), || {
        let remote = host.host().remotes.into_iter().find(|it| it.destination == destination)?;
        match remote.reached? {
            Reached::Unreached(unreached) => Some(unreached),
            _ => None,
        }
    })
}

fn names(host: &remote::Host) -> Vec<&str> {
    let held = host.projects.as_ref().and_then(|projects| projects.held.as_ref());
    let Some(Held::Kept(kept)) = held else {
        return Vec::new();
    };
    let mut names: Vec<&str> = kept.projects.iter().map(|it| it.name.as_str()).collect();
    names.sort_unstable();
    names
}

fn project_named(host: &Host, name: &str) {
    let dir = host.home.path().join(name);
    fs::create_dir(&dir).unwrap();
    project(host, &dir, None);
}

#[cfg(target_os = "linux")]
fn ended(pid: u32) -> bool {
    let stat = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8(stat.stdout).unwrap();
    stat.trim().is_empty() || stat.trim().starts_with('Z')
}

#[cfg(target_os = "macos")]
fn ended(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    let mut system = System::new();
    let pid = Pid::from_u32(pid);
    let basics = ProcessRefreshKind::nothing();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, basics);
    system.process(pid).is_none_or(|process| process.status() == ProcessStatus::Zombie)
}

#[test]
fn a_daemon_says_each_machine_its_hosts_file_names() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    project_named(&far, "work");
    hosts(&here, "nowhere\n-oProxyCommand=sh\nfar\n");
    let _here = start(here.vars()).unwrap();

    let first = reached(&here, "far", |far| !names(far).is_empty());
    assert_eq!(names(&first), ["work"]);
    project_named(&far, "play");
    reached(&here, "far", |far| names(far) == ["play", "work"]);

    let host = here.host();
    assert_eq!(host.unread_hosts, Some(1));
    let remotes = host.remotes;
    let order: Vec<&str> = remotes.iter().map(|it| it.destination.as_str()).collect();
    assert_eq!(order, ["nowhere", "far"]);
    let unreached = unreached(&here, "nowhere");
    let ended = Stopped::Ended(unreached::Ended { code: Some(255) });
    assert_eq!(unreached.stopped, Some(ended), "{}", unreached.why);
    let said = "ssh: Could not resolve hostname nowhere: Name or service not known";
    assert_eq!(unreached.ssh_said.as_deref(), Some(said), "{}", unreached.why);
}

#[test]
fn a_hosts_file_line_that_names_this_machine_says_so_and_runs_no_ssh() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.bin());
    let node = rustix::system::uname().nodename().to_string_lossy().into_owned();
    let named = format!("me@{node}");
    hosts(&here, &format!("{named}\nlocalhost\nnowhere\n"));
    let _here = start(here.vars()).unwrap();
    eventually("ssh fails to nowhere twice", || {
        here.host();
        (ssh.runs("nowhere") >= 2).then_some(())
    });

    let remotes = here.host().remotes;
    for destination in [named.as_str(), "localhost"] {
        let remote = remotes.iter().find(|remote| remote.destination == destination);
        let reached = remote.and_then(|remote| remote.reached.clone());
        assert_eq!(reached, Some(Reached::Here(())), "{remotes:?}");
        assert_eq!(ssh.runs(destination), 0, "no ssh runs to {destination}");
    }
    let order: Vec<&str> = remotes.iter().map(|remote| remote.destination.as_str()).collect();
    assert_eq!(order, [named.as_str(), "localhost", "nowhere"], "{remotes:?}");
}

#[test]
fn a_machine_whose_luned_is_another_build_is_unreached_and_says_both() {
    let here = Host::new();
    let stamp = release::BUILT.map_or(1, |built| built + 1);
    let other = release::spelled(release::VERSION, Some(stamp));
    FakeSsh::new(here.bin()).reaches_a_stand_in("far", &other);
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    let unreached = unreached(&here, "far");
    assert_eq!(unreached.stopped, Some(Stopped::Unmatched(())), "{}", unreached.why);
    let why = &unreached.why;
    assert!(why.starts_with(&format!("luned {other} (pid ")), "{why}");
    assert!(why.ends_with(&format!("is another build than this one, {}", *release::SAID)), "{why}");
}

#[test]
fn what_another_machine_is_told_to_say_comes_with_its_host() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let said = answered(far.vars().command(&LUNE).args(["say", "over here"]));
    assert!(said.status.success(), "{}", String::from_utf8_lossy(&said.stderr));

    let far_host = reached(&here, "far", |far| !far.sayings.is_empty());
    assert_eq!(far_host.sayings.len(), 1);
    assert_eq!(here.host().sayings, [], "it is the other machine's to say");
}

#[test]
fn a_hosts_file_that_does_not_read_leaves_the_machines_as_they_were() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    assert_eq!(here.host().unread_hosts, Some(0));

    let file = here.home.path().join(".local/state/lune/hosts");
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

#[test]
fn a_machine_that_leaves_the_hosts_file_is_watched_no_more() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let bridges = ssh.bridges("far");
    assert_eq!(bridges.len(), 1);
    assert!(!ended(bridges[0]), "the bridge to far runs");

    hosts(&here, "");
    eventually("far leaves", || here.host().remotes.is_empty().then_some(()));
    eventually("the bridge to far ends", || ended(bridges[0]).then_some(()));
}

#[test]
fn a_machine_whose_daemon_ends_is_reached_again() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    drop(far_daemon);
    eventually("far's daemon is started again", || {
        let folder = folder_in(far.runtime.path())?;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(luned::connect(&folder, PATIENT)).ok().map(|_| ())
    });
    project_named(&far, "later");
    reached(&here, "far", |far| names(far) == ["later"]);
    let _again = Detached { host: &far, pid: far.host().version.unwrap().pid };
}

#[test]
fn a_question_while_a_machine_is_reached_again_is_handed_on() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
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
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let on = Some(On { destination: "far".to_owned() });
        lune_waits::awaited("far answers once reached again", daemon.list_skills(skills_on(on))).await
    });
    assert!(checked.is_ok(), "{checked:?}");
    assert_eq!(ssh.runs("far"), runs + 1);
    let _again = Detached { host: &far, pid: far.host().version.unwrap().pid };
}

#[test]
fn a_question_while_a_machine_is_reached_again_is_asked_of_no_other_build() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let stamp = release::BUILT.map_or(1, |built| built + 1);
    ssh.reaches_a_stand_in_slowly("far", &release::spelled(release::VERSION, Some(stamp)), 2);
    let runs = ssh.runs("far");
    drop(far_daemon);
    eventually("ssh runs to far again", || (ssh.runs("far") > runs).then_some(()));
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let checked = runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let on = Some(On { destination: "far".to_owned() });
        lune_waits::awaited("far answers once reached again", daemon.list_skills(skills_on(on))).await
    });
    let refused = checked.expect_err("another build answered");
    assert_eq!(refused.message(), "luned does not reach far now", "{refused:?}");
    let unreached = unreached(&here, "far");
    assert_eq!(unreached.stopped, Some(Stopped::Unmatched(())), "{}", unreached.why);
}

#[test]
fn a_machine_ssh_no_longer_reaches_is_unreached_since_it_last_answered() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
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

#[test]
fn two_daemons_that_watch_each_other_come_to_rest() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    FakeSsh::new(far.bin()).reaches("here", here.vars());
    hosts(&here, "far\n");
    hosts(&far, "here\n");
    let _here = start(here.vars()).unwrap();
    let _far = start(far.vars()).unwrap();
    reached(&here, "far", |_| true);
    reached(&far, "here", |_| true);

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = crate::host::folder_in(here.runtime.path()).unwrap();
    runtime.block_on(lune_waits::awaited("the host comes to rest", async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let mut after = None;
        loop {
            let asked = WatchRequest { after, kept: None, shown: false };
            let watched = daemon.watch(asked).await.unwrap().into_inner();
            if watched.version.is_some() && watched.version == after {
                return;
            }
            after = watched.version;
        }
    }));
}

fn watch_through(host: &Host, span: Span) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = crate::host::folder_in(host.runtime.path()).unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let mut after = None;
        lune_waits::repeated_through(span, "the daemon answers a watch", async || {
            let asked = daemon.watch(WatchRequest { after, kept: None, shown: false }).await;
            after = asked.expect("the daemon ended").into_inner().version;
        })
        .await;
    });
}

pub fn a_daemon_a_client_keeps_keeps_the_machines_it_watches_running(_: Slot) {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let mut far_daemon = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    watch_through(&here, const { Span::of(ALONE.saturating_add(SWEEP.saturating_mul(3))) });
    assert_eq!(far_daemon.exit_status(), None);
    assert_eq!(ssh.bridges("far").len(), 1);
}

pub fn two_daemons_that_watch_each_other_and_nothing_else_end(_: Slot) {
    let here = Host::new();
    let far = Host::new();
    let to_far = FakeSsh::new(here.bin());
    to_far.reaches("far", far.vars());
    let to_here = FakeSsh::new(far.bin());
    to_here.reaches("here", here.vars());
    hosts(&here, "far\n");
    hosts(&far, "here\n");
    let mut here_daemon = start(here.vars()).unwrap();
    let mut far_daemon = start(far.vars()).unwrap();
    reached(&here, "far", |_| true);
    reached(&far, "here", |_| true);

    here_daemon.ended("here's daemon ends, watched by far's alone");
    far_daemon.ended("far's daemon ends, watched by here's alone");
    assert_eq!(to_far.bridges("far").len(), 1);
    assert_eq!(to_here.bridges("here").len(), 1);
}

#[test]
fn what_each_ssh_to_a_machine_says_again_is_said_once() {
    let here = Host::new();
    let ssh = FakeSsh::new(here.bin());
    hosts(&here, "nowhere\n");
    let mut daemon = start(here.vars()).unwrap();
    eventually("ssh runs to nowhere a few times", || {
        here.host();
        (ssh.runs("nowhere") >= 3).then_some(())
    });

    let unresolved = "nowhere: ssh: Could not resolve hostname nowhere";
    daemon.says("luned says ssh does not resolve nowhere", |line| line.contains(unresolved));
    lune_waits::never("luned says it again", || {
        let said = daemon.said();
        (said.iter().filter(|line| line.contains(unresolved)).count() != 1).then(|| said.to_vec())
    });
}

pub fn skills_on(on: Option<On>) -> ListSkillsRequest {
    ListSkillsRequest { on, dir: "/".to_owned(), harness: Some(api::Harness::Pi.into()) }
}

fn refused_question(host: &Host, destination: &str) -> Status {
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let on = Some(On { destination: destination.to_owned() });
        daemon.list_skills(skills_on(on)).await.unwrap_err()
    })
}

#[test]
fn a_question_for_a_machine_not_reached_runs_ssh_to_it_at_once() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    eventually("ssh fails to far five times", || {
        here.host();
        (ssh.unresolved("far") >= 5).then_some(())
    });

    ssh.reaches("far", far.vars());
    let refused = refused_question(&here, "far");
    assert_eq!(refused.code(), Code::Unavailable, "{refused:?}");
    reached(&here, "far", |_| true);
    assert_eq!(ssh.runs("far"), 6);
}

#[test]
fn the_log_says_why_a_machine_is_unreached_as_that_changes() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
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
    refused_question(&here, "far");
    reached(&here, "far", |_| true);

    let said = daemon.says("luned says far is reached", |line| line.contains("far: reached, after "));
    let unreached: Vec<&String> =
        said.iter().filter(|line| line.contains("far: unreached: ")).collect();
    assert_eq!(unreached.len(), 2, "{said:?}");
    assert!(unreached[0].ends_with("(exit status: 255)"), "{said:?}");
    assert!(unreached[1].ends_with("(exit status: 7)"), "{said:?}");
    let reached = said.iter().filter(|line| line.contains("far: reached, after ")).count();
    assert_eq!(reached, 1, "{said:?}");
}

#[test]
fn lune_ssh_adds_a_machine_and_says_how_luned_reaches_it() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "nowhere\n");
    let ssh_to = |destination: &str| {
        let output = here.vars().command(&LUNE).args(["ssh", destination]).output().unwrap();
        let said = String::from_utf8(output.stdout).unwrap();
        (output.status.success(), said, String::from_utf8(output.stderr).unwrap())
    };

    let (ok, said, _) = ssh_to("far");
    assert!(ok);
    assert!(said.starts_with("far: added to lune's hosts file\nfar: luned reaches it once"));
    let file = here.home.path().join(".local/state/lune/hosts");
    assert_eq!(fs::read_to_string(&file).unwrap(), "nowhere\nfar\n");

    let _here = start(here.vars()).unwrap();
    let (ok, said, _) = ssh_to("far");
    let lines: Vec<&str> = said.lines().collect();
    assert!(ok, "{said}");
    assert_eq!(lines[0], "far: lune's hosts file names it already");
    assert!(lines[1].starts_with("far: reached, "), "{said}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "nowhere\nfar\n");

    let (ok, said, stderr) = ssh_to("elsewhere");
    assert!(!ok);
    assert_eq!(said, "elsewhere: added to lune's hosts file\n");
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(lines[0].starts_with("lune: elsewhere: unreached: "), "{stderr}");
    let ssh_said = "ssh: Could not resolve hostname elsewhere: Name or service not known";
    assert_eq!(lines[1..], [format!("lune: ssh said \"{ssh_said}\"")], "{stderr}");
    let log = here.home.path().join(".local/state/lune/lune.log");
    eventually("what ssh said is in the log", || {
        let logged = fs::read_to_string(&log).ok()?;
        logged.contains(&format!("elsewhere: {ssh_said}")).then_some(())
    });

    let node = rustix::system::uname().nodename().to_string_lossy().into_owned();
    for destination in [format!("me@{node}"), "localhost".to_owned()] {
        let (ok, said, stderr) = ssh_to(&destination);
        assert!(!ok);
        assert_eq!(said, "");
        assert_eq!(stderr, format!("lune: {destination}: {}\n", luned::THIS_MACHINE));
        assert_eq!(ssh.runs(&destination), 0, "no ssh runs to {destination}");
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), "nowhere\nfar\nelsewhere\n");
}

#[test]
fn a_daemon_hands_on_what_is_asked_of_another_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    script(&far.bin().join("claude"), "echo '[]'\n");
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
    let pi_skill = dir.path().join(".agents/skills/near-pi");
    fs::create_dir_all(&pi_skill).unwrap();
    fs::write(pi_skill.join("SKILL.md"), "---\ndescription: Near pi.\n---\n").unwrap();
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let dir_of_skills = dir.path().to_str().unwrap().to_owned();
        let skills = ListSkillsRequest { on: on("far"), dir: dir_of_skills.clone(), harness: None };
        let found = daemon.list_skills(skills).await.unwrap();
        let near =
            Skill { name: "near".to_owned(), description: "Near.".to_owned(), only_you: false };
        assert_eq!(found.into_inner().skills, [near]);
        let (dir_of, pi) = (dir.path().to_str().unwrap().to_owned(), Some(api::Harness::Pi.into()));
        let skills = ListSkillsRequest { on: on("far"), dir: dir_of, harness: pi };
        let found = daemon.list_skills(skills).await.unwrap();
        let (name, description) = ("near-pi".to_owned(), "Near pi.".to_owned());
        assert_eq!(found.into_inner().skills, [Skill { name, description, only_you: false }]);
        let invalid =
            |status: Status| assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        let nowhere = ListSkillsRequest { on: on("far"), dir: String::new(), harness: None };
        invalid(daemon.list_skills(nowhere).await.unwrap_err());
        let unreached =
            ListSkillsRequest { on: on("nowhere"), dir: dir_of_skills.clone(), harness: None };
        let unreached = daemon.list_skills(unreached).await.unwrap_err();
        assert_eq!(unreached.code(), Code::Unavailable, "{unreached:?}");

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

pub fn claude_that_takes_turns(host: &Host) {
    let home = host.home.path().display();
    let slow = SLOW.as_secs_f64();
    let text = format!(
        r#"cat > "{home}/words"
{{ echo "cwd=$(pwd -P)"
echo "turn=${{LUNE_TURN-unset}}"; echo "before=${{LUNE_TURN_BEFORE-unset}}"
resumed=none; for arg; do [ "$after" = --resume ] && resumed=$arg; after=$arg; done
echo "resumed=$resumed"; echo "model=$2 $3"
}} > "{home}/told"
echo '{{"type":"system","subtype":"init","session_id":"7c22"}}'
ran='{{"type":"assistant","parent_tool_use_id":null,"message":{{"content":[{{"type":"tool_use",'\
'"id":"toolu_1","name":"Bash","input":{{"command":"lune agents"}}}}]}}}}'
case "$(cat "{home}/words")" in
    *please-fail*)
        echo '{{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}}'
        exit 1 ;;
    *please-be-slow*) sleep {slow} ;;
    *please-get-stuck*) echo "$ran"; echo $$ > "{home}/stuck"; exec sleep 60 ;;
esac
echo "$ran"
echo '{{"type":"result","subtype":"success","is_error":false,"result":"No agent runs."}}'
"#
    );
    script(&host.bin().join("claude"), &text);
}

pub fn told(host: &Host) -> (Vec<String>, String) {
    let told = fs::read_to_string(host.home.path().join("told")).unwrap();
    let words = fs::read_to_string(host.home.path().join("words")).unwrap();
    (told.lines().map(str::to_owned).collect(), words)
}

async fn turn(
    daemon: &mut Daemon,
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

fn ask(on: &str, cwd: Option<&Path>, words: &str, first: bool) -> AskRequest {
    let resumed = (!first).then(|| SessionId::parse("7c22").unwrap());
    let turn = Turn::new(255, (!first).then_some(10)).unwrap();
    let row = "The row highlighted is project work.".to_owned();
    let asked = Asked::new(cwd.map(Path::to_owned), words, resumed, row, turn).unwrap();
    asked.asked(Some(On { destination: on.to_owned() }))
}

#[test]
fn a_turn_of_the_ask_box_runs_on_the_machine_it_is_for() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    claude_that_takes_turns(&far);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let dir = far.home.path().join("w");
    fs::create_dir(&dir).unwrap();
    let (dir_there, home_there) =
        (dir.canonicalize().unwrap(), far.home.path().canonicalize().unwrap());

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let first = ask("far", Some(&dir), "which agents run", true);
        let (steps, ended) = turn(&mut daemon, first).await;
        let ran = Step::Ran { id: "toolu_1".to_owned(), command: "lune agents".to_owned() };
        let session = Step::Session(SessionId::parse("7c22").unwrap());
        let said = Step::Said("No agent runs.".to_owned());
        assert_eq!(steps, [session.clone(), ran.clone(), said.clone()]);
        ended.unwrap();
        let (told_there, words) = told(&far);
        assert_eq!(told_there, [
            format!("cwd={}", dir_there.display()),
            "turn=00000000000000ff".to_owned(),
            "before=unset".to_owned(),
            "resumed=none".to_owned(),
            "model=--model sonnet".to_owned(),
        ]);
        assert_eq!(words, "which agents run");

        let (steps, ended) = turn(&mut daemon, ask("far", None, "yes", false)).await;
        assert_eq!(steps, [session.clone(), ran.clone(), said]);
        ended.unwrap();
        let (told_there, words) = told(&far);
        assert_eq!(told_there[..4], [
            format!("cwd={}", home_there.display()),
            "turn=00000000000000ff".to_owned(),
            "before=000000000000000a".to_owned(),
            "resumed=7c22".to_owned(),
        ]);
        assert_eq!(words, "yes");

        let failing = ask("far", None, "please-fail", true);
        let (steps, ended) = turn(&mut daemon, failing).await;
        assert_eq!(steps, std::slice::from_ref(&session));
        let failed = ended.unwrap_err();
        assert_eq!((failed.code(), failed.message()), (Code::FailedPrecondition, "Not logged in"));

        let stuck = ask("far", None, "please-get-stuck", true);
        let mut steps = daemon.ask(stuck).await.unwrap().into_inner();
        for step in [session, ran] {
            assert_eq!(steps.message().await.unwrap().map(Step::of), Some(Ok(Some(step))));
        }
        let stuck = far.home.path().join("stuck");
        let stuck = lune_waits::eventually_async("claude there says its pid", async || {
            fs::read_to_string(&stuck).ok().and_then(|pid| pid.trim().parse().ok())
        })
        .await;
        let stuck = Pid::from_raw(stuck).unwrap();
        assert!(test_kill_process(stuck).is_ok());
        drop(steps);
        let killed = || async move { test_kill_process(stuck).is_err().then_some(()) };
        lune_waits::eventually_async("claude there is killed", killed).await;

        let invalid =
            |status: Status| assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        let x = || ask("far", None, "x", true);
        let relative = AskRequest { cwd: Some("w".to_owned()), ..x() };
        invalid(turn(&mut daemon, relative).await.1.unwrap_err());
        let blank = AskRequest { words: " ".to_owned(), ..x() };
        invalid(turn(&mut daemon, blank).await.1.unwrap_err());
        let forged = AskRequest { resumed: Some("--debug".to_owned()), ..x() };
        invalid(turn(&mut daemon, forged).await.1.unwrap_err());
        let nowhere = turn(&mut daemon, ask("nowhere", None, "x", true)).await;
        assert_eq!(nowhere.1.unwrap_err().code(), Code::Unavailable);
        assert!(!here.home.path().join("told").exists());
    });
}

#[test]
fn a_turn_of_the_ask_box_there_takes_as_long_as_claude_does() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    claude_that_takes_turns(&far);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let started = Began::now();
        let slow = ask("far", None, "please-be-slow", true);
        let (steps, ended) = turn(&mut daemon, slow).await;
        ended.unwrap();
        assert_eq!(steps.len(), 3, "{steps:?}");
        started.lasted().at_least(SLOW, "a slow turn is answered only once claude ends it");
    });
}

pub fn claude_that_writes_issues(host: &Host) {
    let home = host.home.path().display();
    let text = format!(
        r#"words=$(cat)
printf '%s' "$words" > "{home}/told"
{{ echo "cwd=$(pwd -P)"; echo "view=${{LUNE_MUX_TERMINAL-unset}}"
for arg; do case "$arg" in --*) echo "$arg";; esac; done
}} > "{home}/wrote"
handed=
for arg; do [ "$handed" = --settings ] && cp "$arg" "{home}/settings"; handed=$arg; done
case "$words" in
    *please-fail*)
        echo '{{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}}'
        exit 1 ;;
esac
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"{{}}","structured_output":{{"title":"A red button","body":"It is red.\nToo red.","label":"bug"}}}}'
"#
    );
    script(&host.bin().join("claude"), &text);
}

#[test]
fn an_issue_is_written_and_filed_on_the_machine_it_is_for() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    claude_that_writes_issues(&far);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let dir = far.home.path().join("w");
    fs::create_dir(&dir).unwrap();
    let there = dir.canonicalize().unwrap().display().to_string();
    git_said(&dir, &["init", "--quiet"]);
    git_said(&dir, &["remote", "add", "origin", "git@github.com:me/n.git"]);
    git_said(&dir, &["remote", "add", "lab", "git@gitlab.com:o/n.git"]);
    git_said(&dir, &["remote", "add", "upstream", "https://github.com/o/n"]);
    fs::write(dir.join(".gitignore"), ".env\n").unwrap();
    fs::write(dir.join(".env"), "TOKEN=secret\n").unwrap();

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let on = |destination: &str| Some(On { destination: destination.to_owned() });
        let file = |on, dir: &Path, words: &str| FileIssueRequest {
            on,
            dir: dir.display().to_string(),
            words: words.to_owned(),
            repository: "o/n".to_owned(),
        };
        let listed = IssueRepositoriesRequest { on: on("far"), dir: dir.display().to_string() };
        let listed = daemon.issue_repositories(listed).await.unwrap().into_inner();
        let remote = |remote: &str, repository: &str| IssueRepository {
            remote: remote.to_owned(),
            repository: repository.to_owned(),
        };
        assert_eq!(listed.repositories, [remote("upstream", "o/n"), remote("origin", "me/n")]);
        let relative = IssueRepositoriesRequest { on: on("far"), dir: "w".to_owned() };
        let relative = daemon.issue_repositories(relative).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");

        let filed = daemon.file_issue(file(on("far"), &dir, " a red button ")).await;
        let filed = filed.unwrap().into_inner().message().await.unwrap().unwrap();
        let filed = Filed::of(filed).unwrap();
        assert_eq!((filed.number().get(), filed.title()), (360, "A red button"));
        assert_eq!(filed.link().url(), "https://github.com/o/n/issues/360");
        let wrote = fs::read_to_string(far.home.path().join("wrote")).unwrap();
        let wrote: Vec<&str> = wrote.lines().collect();
        let cwd = format!("cwd={there}");
        assert_eq!(wrote[..2], [cwd.as_str(), "view=unset"]);
        let told = fs::read_to_string(far.home.path().join("told")).unwrap();
        assert!(told.starts_with("a red button\n\n[lune: the repository's labels"), "{told}");
        assert!(told.contains(r#"[{"name":"bug"}]"#), "{told}");
        assert!(told.contains(r#"[{"title":"An older one","#), "{told}");
        let mine = "--author @me --state all --limit 5 --json title,body,labels";
        let listed = [
            format!("{there} label list --limit 100 --json name --repo github.com/o/n"),
            format!("{there} issue list {mine} --repo github.com/o/n"),
        ];
        assert_eq!(far.gh.listed(), listed);
        let options = ["--json-schema", "--no-session-persistence", "--safe-mode", "--settings"];
        for option in options {
            assert!(wrote.contains(&option), "{option} in {wrote:?}");
        }
        let settings = fs::read_to_string(far.home.path().join("settings")).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
        let denied = settings["permissions"]["deny"].as_array().unwrap();
        for rule in [format!("Read(/{there}/.env)"), format!("Read(/{there}/**/.git/**)")] {
            let rule = serde_json::Value::from(rule);
            assert!(denied.contains(&rule), "{rule} in {denied:?}");
        }
        let asked =
            "issue create --title A red button --body-file - --repo github.com/o/n --label bug";
        let created = [there.as_str(), asked, "It is red.", "Too red."].map(str::to_owned);
        assert_eq!(far.gh.created(), Some(created.to_vec()));
        assert_eq!(here.gh.created(), None);

        far.gh.refuse_issues("could not add label: 'bug' not found");
        let refused = daemon.file_issue(file(on("far"), &dir, "a red button")).await;
        let refused = refused.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
        assert_eq!(refused.message(), "gh: could not add label: 'bug' not found");
        far.gh.refuse_issues("HTTP 502: Bad Gateway (https://api.github.com/graphql)");
        let lost = daemon.file_issue(file(on("far"), &dir, "a red button")).await;
        let lost = lost.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(lost.code(), Code::Aborted, "{lost:?}");
        assert_eq!(lost.message(), "gh: HTTP 502: Bad Gateway (https://api.github.com/graphql)");
        far.gh.refuse_issues("could not add label: 'bug' not found");

        far.gh.say_on_create("Created it\n");
        let unread = daemon.file_issue(file(on("far"), &dir, "a red button")).await;
        let unread = unread.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(unread.code(), Code::Aborted, "{unread:?}");
        assert_eq!(unread.message(), r#"gh said "Created it", no issue's page"#);
        fs::remove_file(far.bin().join("pulls/create-says")).unwrap();

        fs::remove_file(far.bin().join("pulls/created")).unwrap();
        let unwritten = daemon.file_issue(file(on("far"), &dir, "please-fail")).await;
        let unwritten = unwritten.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(unwritten.code(), Code::FailedPrecondition, "{unwritten:?}");
        assert_eq!(unwritten.message(), "claude failed: Not logged in");
        assert_eq!(far.gh.created(), None);

        fs::remove_file(far.home.path().join("wrote")).unwrap();
        far.gh.refuse_lists("none of the git remotes here point to a known GitHub host");
        let unlisted = daemon.file_issue(file(on("far"), &dir, "a red button")).await;
        let unlisted = unlisted.unwrap().into_inner().message().await.unwrap_err();
        assert_eq!(unlisted.code(), Code::FailedPrecondition, "{unlisted:?}");
        assert!(unlisted.message().starts_with("gh: none of the git remotes"), "{unlisted:?}");
        assert!(!far.home.path().join("wrote").exists());

        let invalid =
            |status: Status| assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        invalid(daemon.file_issue(file(on("far"), Path::new("w"), "x")).await.unwrap_err());
        invalid(daemon.file_issue(file(on("far"), &dir, " ")).await.unwrap_err());
        let unnamed = FileIssueRequest { repository: String::new(), ..file(on("far"), &dir, "x") };
        invalid(daemon.file_issue(unnamed).await.unwrap_err());
        let nowhere = daemon.file_issue(file(on("nowhere"), &dir, "x")).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::FailedPrecondition, "{nowhere:?}");
        assert_eq!(nowhere.message(), "luned does not reach nowhere now");
        assert_eq!(here.gh.created(), None);
    });
}

#[test]
fn another_machine_s_repositories_are_listed_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let dir = tempfile::tempdir().unwrap();
    let (work, notes) = (dir.path().join("work"), dir.path().join("notes"));
    fs::create_dir_all(work.join(".bare")).unwrap();
    fs::create_dir_all(work.join("main")).unwrap();
    fs::create_dir(&notes).unwrap();
    let owned = far.home.path().join("lune").join("app");
    fs::create_dir_all(owned.join(".bare")).unwrap();
    fs::create_dir_all(far.home.path().join("lune").join("plain")).unwrap();
    let real = |path: &Path| fs::canonicalize(path).unwrap().display().to_string();
    let (clones, owned) = (real(&far.home.path().join("lune")), real(&owned));
    let main = work.join("main").display().to_string();
    let (work, notes) = (real(&work), notes.display().to_string());
    let lists = |dirs: &[&str]| {
        dirs.iter().fold(String::new(), |mut script, dir| {
            writeln!(script, "echo '{dir}'").unwrap();
            script
        })
    };
    script(&far.bin().join("zoxide"), &lists(&[&main, &notes, &work]));
    script(&here.bin().join("zoxide"), &lists(&["/here/only"]));
    script(&far.bin().join("eza"), "echo \"far: $*\"\n");
    script(&here.bin().join("eza"), "echo \"here: $*\"\n");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let listed = ListRepositoriesRequest { on: on("far") };
        let listed = daemon.list_repositories(listed).await.unwrap().into_inner();
        assert_eq!(listed.owned, [clones], "far's own folder for clones");
        let paths: Vec<String> = listed.repositories.into_iter().map(|it| it.path).collect();
        assert_eq!(paths, [owned, work]);
        let nowhere = ListRepositoriesRequest { on: on("nowhere") };
        let nowhere = daemon.list_repositories(nowhere).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::Unavailable, "{nowhere:?}");

        let contents = |dir: &str| ListContentsRequest { on: on("far"), dir: dir.to_owned() };
        let listed = daemon.list_contents(contents(&notes)).await.unwrap().into_inner().listed;
        assert!(listed.starts_with("far: ") && listed.ends_with(&format!(" -- {notes}\n")));
        let relative = daemon.list_contents(contents("notes")).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");
    });
}

fn png(size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((bytes.len()..size).map(|at| (at % 251) as u8));
    bytes
}

fn image_of(on: Option<On>, size: u64) -> KeepImageRequest {
    KeepImageRequest { part: Some(Part::Image(Image { on, size })) }
}

fn bytes_of(bytes: &[u8]) -> KeepImageRequest {
    KeepImageRequest { part: Some(Part::Bytes(bytes.to_vec())) }
}

async fn kept_image(
    daemon: &mut Daemon,
    parts: Vec<KeepImageRequest>,
) -> Result<String, Status> {
    let mut answer = daemon.keep_image(tokio_stream::iter(parts)).await?.into_inner();
    let kept = answer.message().await?.expect("an answer");
    assert_eq!(answer.message().await?, None, "the stream ends after the answer");
    Ok(kept.path)
}

#[test]
fn an_image_is_kept_on_another_machine_through_the_bridge() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let on = |destination: &str| Some(On { destination: destination.to_owned() });
    let folder = folder_in(here.runtime.path()).unwrap();
    let images_in = |host: &Host| {
        let runtime = folder_in(host.runtime.path()).unwrap();
        let found = fs::read_dir(runtime.path().join("images"));
        found.map_or(0, Iterator::count)
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
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
        let kept = fs::read(&path).unwrap();
        assert_eq!(kept.len(), small.len(), "the small image's byte count");
        assert_eq!(kept, small, "the small image came as it went");
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
        let mut client = luned::connect(&folder, PATIENT).await.unwrap();
        let (send, parts) = tokio::sync::mpsc::channel(2);
        send.send(image_of(None, 1024)).await.unwrap();
        send.send(bytes_of(&came)).await.unwrap();
        let _answer = client.keep_image(ReceiverStream::new(parts)).await.unwrap();
        let written = || found().iter().any(|file| fs::metadata(file).unwrap().len() >= came.len() as u64);
        let written = || async move { written().then_some(()) };
        lune_waits::eventually_async("what came of the image is written", written).await;
        stop(daemon.pid());
        let ended = daemon.ended_async("the daemon ends").await;
        assert_eq!(ended.signal(), Some(Signal::TERM.as_raw()), "it runs nothing on its way out");
        let png = |file: &PathBuf| file.extension() == Some("png".as_ref());
        assert!(!found().iter().any(png), "{:?}", found());
    });
}

#[test]
fn a_daemon_that_starts_takes_away_the_files_past_their_day() {
    let (now, slack) = (SystemTime::now(), KEPT / 24);
    let (past_its_day, young) = (now - (KEPT + slack), now - slack);
    let folder = |path: PathBuf| {
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    };
    let file = |path: PathBuf, modified: SystemTime| {
        fs::File::create(&path).unwrap().set_modified(modified).unwrap();
        path
    };

    let host = Host::new();
    let runtime = folder(host.runtime.path().join("lune"));
    let (images, printed) = (folder(runtime.join("images")), folder(runtime.join("printed")));
    let past = [
        file(images.join("past.png"), past_its_day),
        file(images.join("killed.png.part"), past_its_day),
        file(printed.join("past.txt"), past_its_day),
    ];
    let young = [file(images.join("young.png"), young), file(printed.join("young.txt"), young)];
    let _daemon = start(host.vars()).unwrap();
    eventually("the files past their day to go", || {
        past.iter().all(|file| !file.exists()).then_some(())
    });
    assert!(young.iter().all(|file| file.exists()), "{young:?}");

    let linked = Host::new();
    let runtime = folder(linked.runtime.path().join("lune"));
    let elsewhere = tempfile::tempdir().unwrap();
    symlink(elsewhere.path(), runtime.join("images")).unwrap();
    let outside = file(elsewhere.path().join("past.png"), past_its_day);
    let past = file(folder(runtime.join("printed")).join("past.txt"), past_its_day);
    let _daemon = start(linked.vars()).unwrap();
    eventually("the file past its day to go", || (!past.exists()).then_some(()));
    assert!(outside.exists(), "a file through a link stays");
}

async fn pasted_on(
    daemon: &mut Daemon,
    on: &str,
    pasted: &Pasted,
) -> Result<Ready<usize>, Status> {
    let request = pasted.asked(Some(On { destination: on.to_owned() }));
    let mut answer = daemon.paste(request).await?.into_inner();
    let ready = answer.message().await?.expect("an answer");
    assert_eq!(answer.message().await?, None, "the stream ends after the answer");
    Ok(pasted.answered(ready).expect("an answer lune reads"))
}

fn clone_with_issue_7(host: &Host, scratch: &Path) -> PathBuf {
    clone_named_with_issue_7(host, scratch, "n")
}

fn clone_named_with_issue_7(host: &Host, scratch: &Path, name: &str) -> PathBuf {
    host.gh.issue(7, "https://github.com/o/n/issues/7");
    let (up, clone) = (scratch.join("up"), scratch.join(name));
    cloned_as_wt(scratch, &up, &clone);
    let bare = clone.join(".bare");
    let github = "git@github.com:o/n.git";
    git(&bare, &["remote", "add", "upstream", github]);
    git(&bare, &["config", &format!("url.{}.insteadOf", up.display()), github]);
    let main = clone.join("main").display().to_string();
    script(&host.bin().join("zoxide"), &format!("echo '{main}'\n"));
    clone
}

#[test]
fn a_link_pasted_on_another_machine_s_row_is_found_and_made_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let main = clone.join("main").display().to_string();
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let link = Work::Link(issue.clone());
        let worked = |dir: &str| links::Worked { dir: absolute(dir), pull: None, here: false };
        let session = Pasted::new(link.clone(), vec![worked("/nowhere/at/all"), worked(&main)]);
        let ready = pasted_on(&mut daemon, "far", &session).await.unwrap();
        let folder = clone.join("issue-7").display().to_string();
        let branch = Said::new("issue-7".to_owned());
        let work = Readied::Link(issue);
        let answer = Ready {
            at: At::Unopened,
            folder: absolute(&folder),
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
        assert_eq!(made.unwrap(), answer, "the clone zoxide knows, with no view in it");

        let mut relative = session.asked(Some(On { destination: "far".to_owned() }));
        relative.worked[1].dir = "n/main".to_owned();
        let relative = daemon.paste(relative).await.unwrap_err();
        assert_eq!(relative.code(), Code::InvalidArgument, "{relative:?}");
        let nowhere = pasted_on(&mut daemon, "nowhere", &session).await.unwrap_err();
        assert_eq!(nowhere.code(), Code::FailedPrecondition, "{nowhere:?}");
        assert_eq!(far.gh.issues().len(), 2);
    });
}

#[test]
fn a_link_whose_clone_a_folder_s_project_holds_makes_nothing() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let notes = scratch.path().canonicalize().unwrap().join("notes");
    fs::create_dir(&notes).unwrap();
    let clone = clone_with_issue_7(&far, &notes);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    project(&far, &notes, None);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let link = Work::Link(Link::parse("https://github.com/o/n/issues/7").unwrap());
        for worked in [vec![view_in(&clone.join("main"))], Vec::new()] {
            let refused = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), worked)).await;
            let refused = refused.unwrap_err();
            assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
            let notes = notes.display();
            let one = "notes is a folder, and its own is its one worktree";
            let held = format!("{} is in {notes}, project notes's folder; {one}", clone.display());
            assert_eq!(refused.message(), format!("nothing made: {held}"));
        }
    });
    assert!(!clone.join("issue-7").exists(), "no worktree is made for the issue");
    let mut branches = Command::new("git");
    branches.arg("-C").arg(clone.join(".bare")).args(["branch", "--list", "issue-7"]);
    let branches = branches.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout), "", "nor its branch");
    assert!(far.gh.issues().is_empty(), "nor is GitHub asked: {:?}", far.gh.issues());
}

#[test]
fn a_link_whose_clone_no_project_holds_and_none_can_be_named_for_makes_nothing() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let long = "n".repeat(65);
    let clone = clone_named_with_issue_7(&far, &scratch, &long);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let link = Work::Link(Link::parse("https://github.com/o/n/issues/7").unwrap());
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        for worked in [vec![view_in(&clone.join("main"))], Vec::new()] {
            let refused = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), worked)).await;
            let refused = refused.unwrap_err();
            assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
            let unheld = format!("nothing made: no project holds {}", clone.display());
            let unnamed = format!("the name \"{long}\" is longer than 64 characters");
            let why = format!("{unheld}, and one made for it cannot be named: {unnamed}");
            assert_eq!(refused.message(), why);
        }
    });
    assert!(!clone.join("issue-7").exists(), "no worktree is made for the issue");
    let mut branches = Command::new("git");
    branches.arg("-C").arg(clone.join(".bare")).args(["branch", "--list", "issue-7"]);
    let branches = branches.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout), "", "nor its branch");

    project(&far, &clone, Some("n"));
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let made = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), Vec::new())).await;
        let made = made.unwrap();
        assert_eq!(made.at, At::Unopened);
        assert_eq!(made.folder.as_str(), clone.join("issue-7").display().to_string());
    });
    assert!(clone.join("issue-7").join(".git").is_file(), "a clone a project holds keeps its name");
}

#[test]
fn a_link_makes_nothing_in_a_bare_repository_a_session_works_in_and_runs_none_of_its_hooks() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&host, &scratch);
    let (up, thing) = (scratch.join("up"), scratch.join("vendor/thing"));
    let planted = thing.join(".bare");
    let (from, to) = (up.display().to_string(), planted.display().to_string());
    git(&scratch, &["clone", "-q", "--bare", &from, &to]);
    let github = "git@github.com:o/n.git";
    git(&planted, &["remote", "add", "upstream", github]);
    git(&planted, &["config", &format!("url.{}.insteadOf", up.display()), github]);
    let hooked = scratch.join("hooked");
    script(&planted.join("hooks/post-checkout"), &format!("touch '{}'\n", hooked.display()));
    let _daemon = start(host.vars()).unwrap();
    let link = Work::Link(Link::parse("https://github.com/o/n/issues/7").unwrap());
    let pasted = Pasted::new(link, vec![view_in(&planted)]);

    let made = pasted_here(&host, &pasted).unwrap();
    assert_eq!(made.folder.as_str(), clone.join("issue-7").display().to_string(), "zoxide's clone");
    script(&host.bin().join("zoxide"), "exit 0\n");
    let refused = pasted_here(&host, &pasted).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    let why = format!(
        "{} is in a bare repository of o/n, not in a worktree of it; lune reads a bare \
         repository only through its worktrees, as a clone can hold one committed as plain files",
        planted.display()
    );
    assert_eq!(refused.message(), why);
    assert!(!thing.join("issue-7").exists(), "no worktree is made beside it");
    assert!(!hooked.exists(), "its hook never runs");
}

#[test]
fn a_machine_with_none_of_a_link_s_repository_says_so_apart() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    script(&far.bin().join("zoxide"), "exit 0\n");
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
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
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
            dir: absolute(&dir.display().to_string()),
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
        for views in [vec![in_dir(&by_wt.join("main"))], vec![on_it]] {
            let kind = pasted_on(&mut daemon, "far", &Pasted::new(issue(8), views)).await;
            let kind = kind.unwrap_err();
            assert_eq!(kind.code(), Code::FailedPrecondition, "{kind:?}");
            let paste = "it is a pull request on GitHub: paste https://github.com/o/n/pull/8";
            assert_eq!(kind.message(), paste, "{kind:?}");
        }
        assert_eq!(far.gh.issues().len(), 2, "{:?}", far.gh.issues());
    });
}

#[test]
fn ssh_says_what_host_a_remote_names_and_is_asked_of_no_option() {
    let host = Host::new();
    let runs = host.bin().join("ssh-runs");
    let ssh = format!(
        "echo \"$*\" >> {}\nif [ \"$2\" = github-work ]; then echo 'hostname github.com'; else echo \"hostname $2\"; fi\n",
        quoted(&runs.display().to_string())
    );
    script(&host.bin().join("ssh"), &ssh);
    script(&host.bin().join("zoxide"), "exit 0\n");
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
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let link = Work::Link(Link::parse("https://github.com/o/n/pull/7").unwrap());
        let answers = [
            (&option, Code::NotFound, "no session works in o/n"),
            (&renamed, Code::FailedPrecondition, "that git made"),
        ];
        for (dir, code, says) in answers {
            let view = view_in(dir);
            let asked = Pasted::new(link.clone(), vec![view]).asked(None);
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
    luned::local_git(&mut git);
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
    Pasted::new(Work::Link(link), worked).wanting(Wanted::AtHead).unwrap()
}

fn pasted_here(host: &Host, pasted: &Pasted) -> Result<Ready<usize>, Status> {
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let mut answer = daemon.paste(pasted.asked(None)).await?.into_inner();
        let ready = answer.message().await?.expect("an answer");
        Ok(pasted.answered(ready).expect("an answer lune reads"))
    })
}

fn absolute(path: &str) -> Directory {
    Directory::parse(path.to_owned()).unwrap()
}

fn view_in(dir: &Path) -> links::Worked {
    links::Worked { dir: absolute(&dir.display().to_string()), pull: None, here: false }
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
    let pasted = reviewing_pull_7(vec![view_in(&main)]);
    let at_head = |head: &str, how: &str| {
        let ready = pasted_here(&host, &pasted).unwrap();
        assert_eq!(ready.at, At::Unopened, "{how}");
        assert_eq!(ready.folder.as_str(), feat.display().to_string(), "{how}");
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
    let pasted = Pasted::new(Work::Link(link), vec![view_in(&clone.join("main"))]);

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

#[test]
fn r_on_a_far_machine_s_view_syncs_there_or_says_why_it_did_not() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&far, &scratch);
    let (up, feat) = (scratch.join("up"), clone.join("feat"));
    git(&clone.join(".bare"), &["worktree", "add", "-q", "--track", "-b", "feat", "../feat", "origin/feat"]);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let view = links::Worked { pull: Some(link), ..view_in(&feat) };
    let pasted = reviewing_pull_7(vec![view]);
    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
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

#[test]
fn r_at_the_head_moves_nothing_whatever_is_checked_out() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let feat = clone.join("feat");
    git(&clone.join(".bare"), &["worktree", "add", "-q", "--track", "-b", "feat", "../feat", "origin/feat"]);
    let _daemon = start(host.vars()).unwrap();
    let pasted = reviewing_pull_7(vec![view_in(&clone.join("main"))]);
    let head = git_said(&feat, &["rev-parse", "HEAD"]);

    fs::write(feat.join("mine"), "mine").unwrap();
    git(&feat, &["add", "mine"]);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(feat.join("mine")).unwrap(), "mine");

    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let view = links::Worked { pull: Some(link), ..view_in(&feat) };
    git(&feat, &["switch", "-q", "--detach"]);
    let ready = pasted_here(&host, &reviewing_pull_7(vec![view])).unwrap();
    assert_eq!(ready.at, At::There(0));
    assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), head);
}

#[test]
fn r_on_a_view_gone_to_another_branch_starts_no_review() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&host, &scratch);
    let main = clone.join("main");
    git(&main, &["branch", "-q", "--set-upstream-to=origin/main", "main"]);
    let _daemon = start(host.vars()).unwrap();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let view = links::Worked { pull: Some(link), ..view_in(&main) };

    let refused = pasted_here(&host, &reviewing_pull_7(vec![view])).unwrap_err();
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
    pasted_here(&host, &reviewing_pull_7(vec![view_in(&clone.join("main"))])).unwrap();
    assert!(feat.join("sub/file").is_file());

    git(&work, &["rm", "-rq", "sub"]);
    git(&work, &["commit", "-q", "-m", "no sub"]);
    let head = pushed_from_work();
    let link = Link::parse("https://github.com/o/n/pull/7").unwrap();
    let view = links::Worked { pull: Some(link), ..view_in(&feat.join("sub")) };
    let refused = pasted_here(&host, &reviewing_pull_7(vec![view])).unwrap_err();
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
    let pasted = reviewing_pull_7(vec![view_in(&clone.join("main"))]);
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
    let pasted = reviewing_pull_7(vec![view_in(&clone.join("main"))]);
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
    let view = links::Worked { pull: Some(link), ..view_in(&feat) };
    let pasted = reviewing_pull_7(vec![view]);
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
    let pasted = reviewing_pull_7(vec![view_in(&clone.join("main"))]);
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
    let mut luned = host.vars().command(LUNED);
    luned
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.sshCommand")
        .env("GIT_CONFIG_VALUE_0", &ssh);
    let _daemon = start_as(luned).unwrap();
    let pasted = reviewing_pull_7(vec![view_in(&clone.join("main"))]);
    pasted_here(&host, &pasted).unwrap();

    let head = pushed(&host, &up, "feat", "one more");
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&clone.join("feat"), &["rev-parse", "HEAD"]), head);
}

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
    let pasted = reviewing_pull_7(vec![view_in(&main)]);
    let local_main = git_said(&main, &["rev-parse", "main"]);
    let folder = clone.join("f-main");

    let head = opened("from the fork", false);
    let ready = pasted_here(&host, &pasted).unwrap();
    assert_eq!(ready.folder.as_str(), folder.display().to_string());
    assert_eq!(ready.branch.as_deref(), Some("f-main"));
    assert_eq!(git_said(&folder, &["rev-parse", "HEAD"]), head);

    let head = opened("rewritten on the fork", true);
    pasted_here(&host, &pasted).unwrap();
    assert_eq!(git_said(&folder, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_said(&folder, &["rev-parse", "refs/remotes/f/main"]), head);
    assert_eq!(git_said(&main, &["rev-parse", "main"]), local_main);
}

#[test]
fn a_machine_whose_answer_stalls_may_have_made_the_worktree() {
    let here = Host::new();
    let far = Host::new();
    let ssh = FakeSsh::new(here.bin());
    ssh.reaches("far", far.vars());
    ssh.answers_through_a_cat("far");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let stalled = ssh.stalls_answers("far");
    let said = runtime.block_on(async {
        let luned::Reached::Matched(daemon) = luned::reach(&folder).await.unwrap() else {
            panic!("the own build did not match");
        };
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let pasted = Pasted::new(Work::Link(issue), Vec::new());
        pasted.ready_through(&daemon, Some(&"far".parse().unwrap()), luned::Answer::of(GIVES_UP)).await
    });
    drop(stalled);
    let Err(Unready::InDoubt(why)) = said else {
        panic!("{said:?} is not in doubt");
    };
    assert_eq!(why, format!("far: luned did not answer in {}s", GIVES_UP.as_secs()));
    let made = clone.join("issue-7").join(".git");
    eventually("the worktree on far", || made.is_file().then_some(()));
}

#[test]
fn a_file_pasted_on_another_machine_s_row_is_read_and_made_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
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
    git(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let tip = rev_parse(&up, "main");
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let file = |path: &str, from: Option<&str>| {
            let from = from.map(|from| Directory::parse(from.to_owned()).unwrap());
            Work::File(links::File::parse(path, from).unwrap())
        };
        let worked = |dir: &str| links::Worked { dir: absolute(dir), pull: None, here: false };
        let views = vec![worked("/nowhere/at/all"), worked(&main)];
        let asked = Pasted::new(file(&plan, Some("/nowhere/at/all")), views.clone());
        let ready = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let folder = clone.join("md-sync-rework").display().to_string();
        let branch = Said::new("md-sync-rework".to_owned());
        let of_plan = || Readied::File { real: plan.clone() };
        let answer = Ready {
            at: At::Unopened,
            folder: absolute(&folder),
            branch: branch.clone(),
            moved_off: false,
            work: of_plan(),
        };
        assert_eq!(ready, answer);
        assert!(clone.join("md-sync-rework").join(".git").is_file());
        assert_eq!(rev_parse(&clone.join("md-sync-rework"), "HEAD"), tip);
        assert_eq!(far.gh.issues(), [] as [String; 0]);

        let mut in_it = views.clone();
        in_it.push(worked(&folder));
        let asked = Pasted::new(file(&plan, None), in_it);
        let again = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(again.at, At::There(2));
        assert_eq!(again.folder.as_str(), folder);

        let asked = Pasted::new(file(&plan, None), vec![worked("/nowhere/at/all")]);
        let made = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(made, answer, "the file's own clone, with no view in it");

        let home = far.home.path().canonicalize().unwrap();
        fs::write(home.join("errand.md"), "written down").unwrap();
        let asked = Pasted::new(file("~/errand.md", Some(&main)), views.clone());
        let errand = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(errand.at, At::Unopened);
        assert_eq!(errand.folder.as_str(), clone.join("md-errand").display().to_string());
        let real = home.join("errand.md").display().to_string();
        assert_eq!(errand.work, Readied::File { real });

        let asked = Pasted::new(file("docs/plans/Sync Rework.md", Some(&main)), views.clone());
        let relative = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(relative.work, of_plan());
        assert_eq!(relative.folder.as_str(), folder);

        let named = plans.join("main.md");
        fs::write(&named, "written down").unwrap();
        let asked = Pasted::new(file(&named.display().to_string(), None), views.clone());
        let of_main = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(of_main.folder.as_str(), clone.join("md-main").display().to_string());
        assert_eq!(of_main.branch.as_deref(), Some("md-main"));

        let mine = here.home.path().canonicalize().unwrap().join("mine.md");
        fs::write(&mine, "written down").unwrap();
        let asked = Pasted::new(file("~/mine.md", Some(&main)), views.clone());
        assert_eq!(refused_there(&mut daemon, &asked).await, "no such file");
        assert!(!clone.join("md-mine").exists());

        let outside = scratch.join("outside.md");
        fs::write(&outside, "written down").unwrap();
        let asked = Pasted::new(file(&outside.display().to_string(), None), views.clone());
        assert_eq!(
            refused_there(&mut daemon, &asked).await,
            "it is in no repository, and the row highlighted works in no directory to go by"
        );

        let by_git = scratch.join("by-git");
        git(&scratch, &["clone", "-q", &up.display().to_string(), &by_git.display().to_string()]);
        fs::write(by_git.join("plan.md"), "written down").unwrap();
        let in_by_git = by_git.join("plan.md").display().to_string();
        let asked = Pasted::new(file(&in_by_git, None), views.clone());
        assert_eq!(
            refused_there(&mut daemon, &asked).await,
            format!(
                "{} is in a clone git made, and wt would make the worktree in its .git; wt clone \
                 it first",
                by_git.display()
            )
        );
        assert_eq!(rev_parse_verify(&by_git, "refs/heads/md-plan"), None);

        let unread = scratch.join("unread");
        git(&scratch, &["init", "-q", &unread.display().to_string()]);
        git(&unread, &["config", "core.repositoryformatversion", "99"]);
        fs::write(unread.join("in-it.md"), "written down").unwrap();
        let in_it = unread.join("in-it.md").display().to_string();
        let asked = Pasted::new(file(&in_it, Some(&main)), views);
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
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    far.gh.issue(8, "https://github.com/o/n/issues/8");
    far.gh.issue(9, "https://github.com/o/n/issues/9");
    far.gh.issue(10, "https://github.com/o/n/issues/10");
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_issue_7(&far, &scratch);
    let (bare, main, moved) = (clone.join(".bare"), clone.join("main"), clone.join("issue-7"));
    git(&bare, &["worktree", "add", "-q", "../issue-7", "feat"]);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let issue = Link::parse("https://github.com/o/n/issues/7").unwrap();
        let link = Work::Link(issue.clone());
        let views = vec![view_in(&main)];
        let asked = Pasted::new(link.clone(), views.clone());
        let folder = moved.display().to_string();

        let opened = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let (at, work) = (At::Unopened, Readied::Link(issue));
        let branch = Said::new("feat".to_owned());
        let answer = Ready { at, folder: absolute(&folder), branch, moved_off: true, work };
        assert_eq!(opened, answer);
        assert_eq!(rev_parse_verify(&main, "refs/heads/issue-7"), None);

        let in_it = vec![view_in(&main), view_in(&moved)];
        let there = pasted_on(&mut daemon, "far", &Pasted::new(link.clone(), in_it)).await;
        assert_eq!(there.unwrap().at, At::There(1));

        let none = Pasted::new(link.clone(), Vec::new());
        let made = pasted_on(&mut daemon, "far", &none).await.unwrap();
        assert_eq!(made, answer, "the clone zoxide knows, with no view in it");

        git(&moved, &["switch", "-q", "--detach"]);
        let detached = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!((detached.folder, detached.branch), (absolute(&folder), None));
        assert!(detached.moved_off);

        git(&moved, &["switch", "-q", "-c", "issue-7"]);
        let on_it = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(on_it.folder.as_str(), folder);
        assert_eq!(on_it.branch.as_deref(), Some("issue-7"));
        assert!(!on_it.moved_off);

        git(&moved, &["switch", "-q", "--detach"]);
        git(&bare, &["worktree", "add", "-q", "../beside", "issue-7"]);
        let beside = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        assert_eq!(beside.folder.as_str(), clone.join("beside").display().to_string());
        assert!(!beside.moved_off);

        fs::create_dir(clone.join("issue-8")).unwrap();
        let eight = Work::Link(Link::parse("https://github.com/o/n/issues/8").unwrap());
        let why = refused_there(&mut daemon, &Pasted::new(eight, views.clone())).await;
        assert!(why.ends_with("issue-8 is already present"), "{why}");

        git(&bare, &["worktree", "add", "-q", "-b", "gone", "../issue-10", "main"]);
        fs::remove_dir_all(clone.join("issue-10")).unwrap();
        fs::create_dir(clone.join("issue-10")).unwrap();
        let ten = Work::Link(Link::parse("https://github.com/o/n/issues/10").unwrap());
        let why = refused_there(&mut daemon, &Pasted::new(ten, views.clone())).await;
        assert!(why.ends_with("issue-10 is already present"), "{why}");

        let plan = main.join("plan.md");
        fs::write(&plan, "written down").unwrap();
        let plan = plan.display().to_string();
        git(&bare, &["worktree", "add", "-q", "-b", "other", "../md-plan", "main"]);
        let moved = clone.join("md-plan").display().to_string();
        let file = Work::File(links::File::parse(&plan, None).unwrap());
        let asked = Pasted::new(file, views.clone());
        let opened = pasted_on(&mut daemon, "far", &asked).await.unwrap();
        let (at, work) = (At::Unopened, Readied::File { real: plan.clone() });
        let branch = Said::new("other".to_owned());
        assert_eq!(opened, Ready { at, folder: absolute(&moved), branch, moved_off: true, work });
        assert_eq!(rev_parse_verify(&main, "refs/heads/md-plan"), None);

        git(&bare, &["worktree", "add", "-q", "-b", "nine", "../issue-9", "main"]);
        let other = Work::Link(Link::parse("https://github.com/o/n/issues/9").unwrap());
        let other = Pasted::new(other, views);
        assert!(pasted_on(&mut daemon, "far", &other).await.unwrap().moved_off);
        let on = Some(On { destination: "far".to_owned() });
        let older = PasteRequest { reads_moved_off: false, ..other.asked(on) };
        let mut answer = daemon.paste(older).await.unwrap().into_inner();
        let refused = answer.message().await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
        assert!(refused.message().ends_with("issue-9 is already present"), "{refused:?}");
    });
}

#[test]
fn a_worktree_where_a_pull_request_s_would_go_is_taken_off_its_head_branch() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let clone = clone_with_pull_7(&far, &scratch);
    let (up, bare, main, feat) =
        (scratch.join("up"), clone.join(".bare"), clone.join("main"), clone.join("feat"));
    git(&bare, &["worktree", "add", "-q", "-b", "stacked", "../feat", "main"]);
    let was = git_said(&feat, &["rev-parse", "HEAD"]);
    let top = git_said(&up, &["commit-tree", "-p", "feat", "-m", "top", "feat^{tree}"]);
    git(&up, &["update-ref", "refs/heads/stack/top", &top]);
    git(&up, &["update-ref", "refs/pull/8/head", &top]);
    git(&bare, &["worktree", "add", "-q", "-b", "lower", "../stack-top", "main"]);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let pull = Link::parse("https://github.com/o/n/pull/7").unwrap();
        let views = vec![view_in(&main)];
        let w = Pasted::new(Work::Link(pull.clone()), views.clone());
        let folder = absolute(&feat.display().to_string());
        let (at, work) = (At::Unopened, Readied::Link(pull.clone()));
        let branch = Said::new("stacked".to_owned());
        let answer = Ready { at, folder: folder.clone(), branch, moved_off: true, work };
        assert_eq!(pasted_on(&mut daemon, "far", &w).await.unwrap(), answer);
        assert_eq!(rev_parse_verify(&main, "refs/heads/feat"), None, "no branch is made");

        let in_it = Pasted::new(Work::Link(pull.clone()), vec![view_in(&main), view_in(&feat)]);
        assert_eq!(pasted_on(&mut daemon, "far", &in_it).await.unwrap().at, At::There(1));

        pushed(&far, &up, "feat", "second");
        let r = reviewing_pull_7(views.clone());
        assert_eq!(pasted_on(&mut daemon, "far", &r).await.unwrap(), answer);
        assert_eq!(git_said(&feat, &["rev-parse", "HEAD"]), was, "r moves no branch");
        assert_eq!(far.gh.viewed(), [] as [String; 0], "r syncs nothing");

        let eight = Link::parse("https://github.com/o/n/pull/8").unwrap();
        let slashed = Pasted::new(Work::Link(eight), views.clone());
        let lower = pasted_on(&mut daemon, "far", &slashed).await.unwrap();
        let stack_top = clone.join("stack-top").display().to_string();
        let lowered = (absolute(&stack_top), Said::new("lower".to_owned()));
        assert_eq!((lower.folder, lower.branch), lowered);
        assert!(lower.moved_off);

        git(&feat, &["switch", "-q", "--detach"]);
        let detached = pasted_on(&mut daemon, "far", &w).await.unwrap();
        assert_eq!((detached.folder, detached.branch), (folder.clone(), None));
        assert!(detached.moved_off);

        let on = Some(On { destination: "far".to_owned() });
        let older = PasteRequest { reads_moved_off: false, ..w.asked(on) };
        let mut answer = daemon.paste(older).await.unwrap().into_inner();
        let refused = answer.message().await.unwrap_err();
        assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
        assert!(refused.message().ends_with("feat is already present"), "{refused:?}");
        let mut luned = far.vars().command(LUNED);
        luned::local_git(&mut luned);
        let made = luned.args(["worktree", "--repo"]).arg(&main).arg(pull.url()).output();
        let made = made.unwrap();
        let stderr = String::from_utf8_lossy(&made.stderr);
        assert!(!made.status.success(), "{stderr}");
        assert!(stderr.trim_end().ends_with("feat is already present"), "{stderr}");

        git(&feat, &["switch", "-q", "--track", "-c", "feat", "origin/feat"]);
        let on_it = pasted_on(&mut daemon, "far", &w).await.unwrap();
        assert_eq!((on_it.folder, on_it.branch), (folder, Said::new("feat".to_owned())));
        assert!(!on_it.moved_off);

        git(&bare, &["worktree", "remove", "--force", "../feat"]);
        fs::create_dir(&feat).unwrap();
        let why = refused_there(&mut daemon, &w).await;
        assert!(why.ends_with("feat is already present"), "{why}");
    });
}

async fn refused_there(daemon: &mut Daemon, pasted: &Pasted) -> String {
    let refused = pasted_on(daemon, "far", pasted).await.unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    refused.message().to_owned()
}

fn rev_parse(dir: &Path, what: &str) -> String {
    rev_parse_verify(dir, what).expect("what git knows")
}

fn rev_parse_verify(dir: &Path, what: &str) -> Option<String> {
    let said = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "--quiet", what])
        .output()
        .unwrap();
    said.status.success().then(|| String::from_utf8(said.stdout).unwrap().trim().to_owned())
}
