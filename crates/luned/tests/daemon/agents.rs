use std::collections::HashSet;
use std::fs::{self, Permissions};
#[cfg(target_os = "linux")]
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command};
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use lune_waits::{eventually, Began, Gap, Span};
use luned::api::agent::{Status as Reported, WaitingFor as Waits};
use luned::api::{self, Agent, ListAgentsResponse, NameSource, Status, UnreadFile, WaitingFor};
use luned::mux::{OwnMade, TerminalKey};
use luned::{SETTLE, SWEEP, TORN};
use serde_json::json;
use tempfile::TempDir;

use crate::host::{start, Running, Vars, LUNE, PATIENT};

pub struct Host {
    runtime: TempDir,
    home: TempDir,
}

impl Host {
    pub fn new() -> Self {
        let host =
            Host { runtime: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() };
        fs::create_dir_all(host.sessions()).unwrap();
        host
    }

    pub fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            home: Some(self.home.path()),
            ..Vars::default()
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.path().join(".claude/sessions")
    }

    pub fn runtime(&self) -> &Path {
        self.runtime.path()
    }

    pub fn logged(&self) -> String {
        let log = self.home.path().join(".local/state/lune/lune.log");
        fs::read_to_string(log).unwrap_or_default()
    }

    pub fn pi_folder(&self) -> PathBuf {
        self.home.path().join(".pi/agent")
    }

    fn file_of(&self, pid: u32) -> PathBuf {
        self.sessions().join(format!("{pid}.json"))
    }

    pub fn write(&self, pid: u32, file: &serde_json::Value) {
        fs::write(self.file_of(pid), file.to_string()).unwrap();
    }

    pub fn list(&self) -> ListAgentsResponse {
        let folder =
            crate::host::folder_in(self.runtime.path()).expect("no daemon has made its folder");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon.list_agents(()).await.unwrap().into_inner()
        })
    }

    pub fn agents(&self) -> Vec<Agent> {
        self.list().agents
    }

    pub fn agent(&self, id: &str) -> Option<Agent> {
        self.agents().into_iter().find(|agent| agent.session_id == id)
    }
}

pub const MADE_NANOS: u64 = 60;

pub fn process(terminal: Option<u32>) -> Running {
    let mut command = Command::new("sleep");
    command.arg("600");
    command.env_remove("LUNE_MUX_TERMINAL").env_remove("LUNE_MUX_TERMINAL_MADE");
    if let Some(id) = terminal {
        command.env("LUNE_MUX_TERMINAL", id.to_string());
        command.env("LUNE_MUX_TERMINAL_MADE", MADE_NANOS.to_string());
    }
    Running::spawn(&mut command)
}

pub struct Hidden {
    pub pid: u32,
    _runs: Option<(Running, ChildStdin, TempDir)>,
}

#[cfg(target_os = "linux")]
pub fn hidden() -> Hidden {
    let scratch = tempfile::tempdir().unwrap();
    let sh = scratch.path().join("sh");
    let path = std::env::var_os("PATH").expect("the tests run with a PATH");
    let found = std::env::split_paths(&path).map(|dir| dir.join("sh")).find(|it| it.is_file());
    let installed = Command::new("install").arg("-m111").arg(found.unwrap()).arg(&sh).status();
    assert!(installed.unwrap().success());
    let mut waits = Command::new(&sh);
    waits.args(["-c", "echo; read -r line"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut hidden = Running::spawn(&mut waits);
    let (waiting, mut said) = hidden.piped();
    said.read_exact(&mut [0; 1]).expect("it says it runs");
    Hidden { pid: hidden.pid(), _runs: Some((hidden, waiting, scratch)) }
}

#[cfg(target_os = "macos")]
pub fn hidden() -> Hidden {
    let listed = Command::new("ps").args(["-axo", "pid=,uid="]).output().unwrap();
    let listed = String::from_utf8(listed.stdout).unwrap();
    let root_s = listed.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse::<u32>().ok()?;
        (pid > 1 && fields.next()? == "0").then_some(pid)
    });
    Hidden { pid: root_s.min().expect("root runs more than launchd"), _runs: None }
}

#[cfg(target_os = "linux")]
fn started(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 1..];
    after.split_whitespace().nth(19).unwrap().to_owned()
}

#[cfg(target_os = "macos")]
fn started(pid: u32) -> String {
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-o", "lstart=", "-p"])
        .arg(pid.to_string())
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn now() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap() + 1
}

fn file(id: &str, pid: u32, start: &str, status: &str) -> serde_json::Value {
    json!({
        "pid": pid,
        "sessionId": id,
        "cwd": "/home/you/work",
        "procStart": start,
        "status": status,
        "statusUpdatedAt": 1_792_859_293_927_u64,
        "startedAt": now(),
        "name": format!("{id}-name"),
        "nameSource": "derived",
        "entrypoint": "cli",
        "kind": "interactive",
    })
}

pub fn file_for(id: &str, process: &Running, status: &str) -> serde_json::Value {
    file_of_pid(id, process.pid(), status)
}

pub fn file_of_pid(id: &str, pid: u32, status: &str) -> serde_json::Value {
    file(id, pid, &started(pid), status)
}

pub fn terminal(id: u32) -> api::TerminalKey {
    let id = std::num::NonZeroU32::new(id).unwrap();
    TerminalKey { id, made: OwnMade::from_nanos(MADE_NANOS) }.into()
}

fn known(status: Status) -> Reported {
    Reported::KnownStatus(status.into())
}

#[test]
fn the_daemon_lists_the_agents_whose_files_name_a_live_process() {
    let host = Host::new();
    let inside = process(Some(3));
    let outside = process(None);
    let (reused, foreign) = (process(None), process(None));
    let crashed = process(None);
    let (crashed_pid, crashed_start) = (crashed.pid(), started(crashed.pid()));
    drop(crashed);

    host.write(inside.pid(), &file_for("inside", &inside, "busy"));
    host.write(outside.pid(), &file_for("outside", &outside, "idle"));
    host.write(crashed_pid, &file("crashed", crashed_pid, &crashed_start, "busy"));
    host.write(reused.pid(), &file("reused", reused.pid(), "0", "busy"));
    let mut elsewhere = file_for("foreign", &foreign, "busy");
    elsewhere["pidDomain"] = json!("linux:elsewhere:pid:[1]");
    host.write(foreign.pid(), &elsewhere);
    let copy = |name: String| {
        fs::copy(host.file_of(outside.pid()), host.sessions().join(name)).unwrap();
    };
    copy(format!("{}-before.json", outside.pid()));
    copy(format!("0{}.json", outside.pid()));
    let _daemon = start(host.vars()).unwrap();

    let mut listed = host.agents();
    listed.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    let seen: Vec<_> = listed
        .iter()
        .map(|agent| {
            let status = agent.status.clone();
            (agent.session_id.as_str(), agent.pid, status, agent.terminal)
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("inside", inside.pid(), Some(known(Status::Busy)), Some(terminal(3))),
            ("outside", outside.pid(), Some(known(Status::Idle)), None),
        ]
    );
    let name = listed[0].name.as_ref().unwrap();
    assert_eq!((name.text.as_str(), name.source()), ("inside-name", NameSource::Derived));
    let entrypoint = listed[0].entrypoint.as_deref();
    assert_eq!((listed[0].cwd.as_str(), entrypoint), ("/home/you/work", Some("cli")));
}

#[test]
fn the_daemon_says_what_claude_code_says_and_no_more() {
    let host = Host::new();
    let (shell, newer, older) = (process(None), process(None), process(None));
    let waits = process(None);
    let mut said = file_for("shell", &shell, "shell");
    said["nameSource"] = json!("user");
    said["entrypoint"] = json!("sdk-cli");
    host.write(shell.pid(), &said);
    let mut said = file_for("newer", &newer, "Compacting");
    said["nameSource"] = json!("someone");
    said["entrypoint"] = json!("claude-vscode");
    said["waitingFor"] = json!("input needed");
    host.write(newer.pid(), &said);
    let mut said = file_for("waits", &waits, "WAITING");
    said["waitingFor"] = json!("Sandbox Request");
    host.write(waits.pid(), &said);
    let mut said = file_for("older", &older, "");
    for unsaid in ["status", "statusUpdatedAt", "nameSource", "entrypoint", "waitingFor"] {
        said.as_object_mut().unwrap().remove(unsaid);
    }
    host.write(older.pid(), &said);
    let _daemon = start(host.vars()).unwrap();

    let mut listed = host.agents();
    listed.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    let seen: Vec<_> = listed
        .iter()
        .map(|agent| {
            let name = agent.name.as_ref();
            let source = name.and_then(|name| name.source.is_some().then(|| name.source()));
            let (id, dated) = (agent.session_id.as_str(), agent.status_since.is_some());
            let entrypoint = agent.entrypoint.as_deref();
            (id, agent.status.clone(), dated, source, entrypoint, agent.waiting_for.clone())
        })
        .collect();
    let other = |word: &str| Some(Reported::OtherStatus(word.to_owned()));
    let (shell, waiting) = (Some(known(Status::Shell)), Some(known(Status::Waiting)));
    let sandbox = Some(Waits::OtherWaitingFor("Sandbox Request".to_owned()));
    let newer = (Some(NameSource::Other), Some("claude-vscode"));
    assert_eq!(
        seen,
        [
            ("newer", other("Compacting"), true, newer.0, newer.1, None),
            ("older", None, false, None, None, None),
            ("shell", shell, true, Some(NameSource::User), Some("sdk-cli"), None),
            ("waits", waiting, true, Some(NameSource::Derived), Some("cli"), sandbox),
        ]
    );
}

#[test]
fn the_daemon_follows_the_files_as_they_change() {
    let host = Host::new();
    let (one, two, three) = (process(None), process(None), process(None));
    let (one_start, two_start) = (started(one.pid()), started(two.pid()));
    host.write(one.pid(), &file("one", one.pid(), &one_start, "busy"));
    host.write(two.pid(), &file("two", two.pid(), &two_start, "idle"));
    let _daemon = start(host.vars()).unwrap();

    let mut waiting = file("one", one.pid(), &one_start, "waiting");
    waiting["waitingFor"] = json!("Permission Prompt");
    host.write(one.pid(), &waiting);
    let waits = Some(known(Status::Waiting));
    let asks = Some(Waits::KnownWaitingFor(WaitingFor::PermissionPrompt.into()));
    let asking = |agent: &Agent| agent.status == waits && agent.waiting_for == asks;
    eventually("one waits", || host.agent("one").filter(asking));

    let whole = file("one", one.pid(), &one_start, "idle").to_string();
    let torn = Began::now();
    fs::write(host.file_of(one.pid()), &whole[..whole.len() / 2]).unwrap();
    let made = file_for("three", &three, "busy").to_string();
    fs::write(host.file_of(three.pid()), &made[..made.len() / 2]).unwrap();
    host.write(two.pid(), &file("two", two.pid(), &two_start, "busy"));
    let works = Some(known(Status::Busy));
    eventually("two works", || host.agent("two").filter(|agent| agent.status == works));
    let listed = host.list();
    let one_does = listed.agents.into_iter().find(|agent| agent.session_id == "one").map(|agent| agent.status);
    if torn.lasted().is_under(TORN) {
        assert_eq!((one_does, listed.unread), (Some(waits), 0));
    } else {
        assert!(listed.unread <= 2, "only the two torn files go unread: {}", listed.unread);
        match one_does {
            Some(status) => assert_eq!(status, waits, "a torn file keeps what it said before"),
            None => assert!(listed.unread >= 1, "one is gone from the list without going unread"),
        }
    }

    fs::remove_file(host.file_of(one.pid())).unwrap();
    eventually("one is gone", || host.agent("one").is_none().then_some(()));

    let left = host.file_of(two.pid());
    drop(two);
    assert!(left.exists());
    eventually("two is gone", || host.agent("two").is_none().then_some(()));
}

#[test]
fn an_agent_that_crashes_leaves_the_list_as_it_ends() {
    let host = Host::new();
    let mut before = process(None);
    host.write(before.pid(), &file_for("before", &before, "busy"));
    let _daemon = start(host.vars()).unwrap();
    eventually("the one before is there", || host.agent("before"));

    let gone = |id: &str| eventually(id, || host.agent(id).is_none().then_some(()));
    before.kill();
    gone("before");

    let mut kept = vec![before];
    for round in 0..4 {
        let id = format!("since-{round}");
        let mut agent = process(None);
        host.write(agent.pid(), &file_for(&id, &agent, "busy"));
        eventually(&id, || host.agent(&id));
        if round % 2 == 0 {
            agent.kill();
            kept.push(agent);
        } else {
            drop(agent);
        }
        gone(&id);
    }
}

#[test]
fn a_file_the_daemon_cannot_read_is_counted_and_not_kept() {
    let host = Host::new();
    let (cut, newer, folder, long) = (process(None), process(None), process(None), process(None));
    let (unnamed, nowhere, relative) = (process(None), process(None), process(None));
    host.write(cut.pid(), &file_for("cut", &cut, "busy"));
    let _daemon = start(host.vars()).unwrap();
    assert!(host.agent("cut").is_some());

    let whole = file_for("cut", &cut, "idle").to_string();
    fs::write(host.file_of(cut.pid()), &whole[..whole.len() / 2]).unwrap();
    let mut shape = file_for("newer", &newer, "busy");
    shape["procStart"] = json!({ "ticks": started(newer.pid()) });
    host.write(newer.pid(), &shape);
    fs::create_dir(host.file_of(folder.pid())).unwrap();
    let padded = " ".repeat(64 * 1024) + &file_for("long", &long, "busy").to_string();
    fs::write(host.file_of(long.pid()), padded).unwrap();
    host.write(unnamed.pid(), &file_for("", &unnamed, "busy"));
    let mut blank = file_for("nowhere", &nowhere, "busy");
    blank["cwd"] = json!("");
    host.write(nowhere.pid(), &blank);
    let mut near = file_for("relative", &relative, "busy");
    near["cwd"] = json!("w");
    host.write(relative.pid(), &near);
    let unread = eventually("seven files are unread", || {
        let listed = host.list();
        (listed.unread == 7).then_some(listed)
    });
    assert!(unread.agents.is_empty(), "{:?}", unread.agents);

    let output = host.vars().command(LUNE).arg("agents").output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("cannot read 7 of Claude Code's session files"), "{said}");

    let left = host.file_of(newer.pid());
    drop(newer);
    assert!(left.exists());
    eventually("six files are unread", || (host.list().unread == 6).then_some(()));
}

#[test]
fn a_file_that_does_not_read_says_nothing_of_its_pid_s_terminal() {
    let host = Host::new();
    let hidden = hidden();
    fs::write(host.file_of(hidden.pid), "{").unwrap();
    let _daemon = start(host.vars()).unwrap();

    eventually("its file is unread", || (host.list().unread == 1).then_some(()));
    let log = host.logged();
    let unread = format!("{} does not read as a session file", host.file_of(hidden.pid).display());
    assert!(log.contains(&unread), "{log}");
    assert!(!log.contains(&format!("agent {} shows in no view", hidden.pid)), "{log}");
}

#[test]
fn a_file_that_does_not_read_is_counted_at_once_where_its_process_runs() {
    let host = Host::new();
    let (inside, outside) = (process(Some(3)), process(None));
    for running in [&inside, &outside] {
        let mut shape = file_for("newer", running, "busy");
        shape["procStart"] = json!({ "ticks": started(running.pid()) });
        host.write(running.pid(), &shape);
    }
    let _daemon = start(host.vars()).unwrap();
    let listed = host.list();
    let in_it = UnreadFile { pid: inside.pid(), terminal: Some(terminal(3)) };
    let elsewhere = UnreadFile { pid: outside.pid(), terminal: None };
    let mut files = vec![in_it, elsewhere];
    files.sort_by_key(|file| file.pid);
    assert_eq!((listed.unread, listed.unread_files), (2, files));
}

#[test]
fn a_file_that_does_not_read_stops_being_counted_as_its_process_ends() {
    let host = Host::new();
    let mut running = process(None);
    let mut shape = file_for("newer", &running, "busy");
    shape["procStart"] = json!({ "ticks": started(running.pid()) });
    host.write(running.pid(), &shape);
    let _daemon = start(host.vars()).unwrap();
    assert_eq!(host.list().unread, 1);

    running.kill();
    eventually("the file is not counted", || (host.list().unread == 0).then_some(()));
}

struct Stopping<'a>(&'a AtomicBool);

impl Drop for Stopping<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_folder_that_never_settles_is_read_anyway() {
    let host = Host::new();
    let one = process(None);
    host.write(one.pid(), &file_for("one", &one, "busy"));
    let _daemon = start(host.vars()).unwrap();

    let stop = AtomicBool::new(false);
    let path = host.sessions().join("churn");
    let churned = const { Gap::of(SETTLE.checked_div(5).expect("a settle divides by five")) };
    thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                fs::write(&path, "churn").unwrap();
                lune_waits::apart(churned);
            }
        });
        let stopping = Stopping(&stop);
        host.write(one.pid(), &file_for("one", &one, "idle"));
        let idle = Some(known(Status::Idle));
        eventually("one is idle", || host.agent("one").filter(|agent| agent.status == idle));
        drop(stopping);
    });
}

#[test]
fn a_folder_the_daemon_cannot_read_it_says_so_once() {
    let host = Host::new();
    fs::set_permissions(host.sessions(), Permissions::from_mode(0o000)).unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let sessions = host.sessions().display().to_string();
    let denied = |line: &str| line.contains(&sessions) && line.ends_with("Permission denied (os error 13)");
    daemon.says("the daemon says it cannot read the folder", denied);
    let sweeps = const { Span::of(SWEEP.saturating_mul(2)) };
    lune_waits::through(sweeps, "the daemon says the same twice", || {
        let about: Vec<&String> = daemon.said().iter().filter(|line| line.contains(&sessions)).collect();
        let once: HashSet<&&String> = about.iter().collect();
        (once.len() != about.len()).then(|| about.iter().map(|line| (*line).clone()).collect::<Vec<_>>())
    });
    fs::set_permissions(host.sessions(), Permissions::from_mode(0o755)).unwrap();

    assert_eq!(host.agents(), [] as [Agent; 0]);
}
