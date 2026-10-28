//! The agents the daemon finds: Claude Code's session files, the processes
//! they name, and the zellij panes those run in.

use std::collections::HashSet;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lunetad::api::agent::Status as Reported;
use lunetad::api::{
    Agent, ListAgentsRequest, ListAgentsResponse, NameSource, Status, UnreadFile, Zellij,
};
use serde_json::json;
use tempfile::TempDir;

use crate::host::{eventually, start, FakeZellij, Running, Vars, LUNETA};

/// The folders of a host of a test's own: the daemon's, a home with Claude
/// Code's session files in it, and zellij's sockets, with a zellij that lists
/// no panes.
pub struct Host {
    runtime: TempDir,
    home: TempDir,
    zellij: TempDir,
    pub lists: FakeZellij,
}

impl Host {
    pub fn new() -> Self {
        let host = Host {
            runtime: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            zellij: tempfile::tempdir().unwrap(),
            lists: FakeZellij::new(),
        };
        fs::create_dir_all(host.sessions()).unwrap();
        fs::create_dir_all(host.contract()).unwrap();
        host
    }

    pub fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            home: Some(self.home.path()),
            zellij: Some(self.zellij.path()),
            bin: Some(self.lists.bin()),
            ..Vars::default()
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.path().join(".claude/sessions")
    }

    /// The folder zellij 0.45 keeps its sessions' sockets in.
    pub fn contract(&self) -> PathBuf {
        self.zellij.path().join("contract_version_1")
    }

    /// Where Claude Code keeps the session file of the agent whose pid is `pid`.
    fn file_of(&self, pid: u32) -> PathBuf {
        self.sessions().join(format!("{pid}.json"))
    }

    /// Writes a session file as Claude Code does, for the agent whose pid is
    /// `pid`.
    pub fn write(&self, pid: u32, file: &serde_json::Value) {
        fs::write(self.file_of(pid), file.to_string()).unwrap();
    }

    /// What the daemon answers.
    pub fn list(&self) -> ListAgentsResponse {
        let folder =
            crate::host::folder_in(self.runtime.path()).expect("no daemon has made its folder");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&folder).await.unwrap();
            daemon.list_agents(ListAgentsRequest {}).await.unwrap().into_inner()
        })
    }

    /// What the daemon lists.
    pub fn agents(&self) -> Vec<Agent> {
        self.list().agents
    }

    /// The agent the daemon lists for the session `id`.
    pub fn agent(&self, id: &str) -> Option<Agent> {
        self.agents().into_iter().find(|agent| agent.session_id == id)
    }
}

/// A process to stand for an agent, in the pane `pane` names, if any.
pub fn process(pane: Option<(&str, u32)>) -> Running {
    let mut command = Command::new("sleep");
    command.arg("600").env_remove("ZELLIJ_SESSION_NAME").env_remove("ZELLIJ_PANE_ID");
    if let Some((session, id)) = pane {
        command.env("ZELLIJ_SESSION_NAME", session).env("ZELLIJ_PANE_ID", id.to_string());
    }
    Running::spawn(&mut command)
}

/// When `pid` started, as Claude Code records it on Linux: field 22 of its
/// `stat` (proc(5)), counted from the `)` that ends its name.
#[cfg(target_os = "linux")]
fn started(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 1..];
    after.split_whitespace().nth(19).unwrap().to_owned()
}

/// When `pid` started, as Claude Code records it on macOS: `ps -o lstart`, in
/// UTC and the C locale.
#[cfg(target_os = "macos")]
fn started(pid: u32) -> String {
    let output = Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-o", "lstart=", "-p"])
        .arg(pid.to_string())
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Now, in the whole milliseconds Claude Code dates an agent's start in, and
/// rounded up: never before a socket made ahead of it.
fn now() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap() + 1
}

/// A session file for the agent `id`, whose process is `pid` and started at
/// `start`, written as the agent starts.
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

/// The file of a process that runs, as it starts.
pub fn file_for(id: &str, process: &Running, status: &str) -> serde_json::Value {
    file(id, process.pid(), &started(process.pid()), status)
}

fn zellij(named: &str, session: Option<&str>, pane: u32) -> Zellij {
    Zellij { named: named.to_owned(), session: session.map(str::to_owned), pane }
}

/// The session the daemon says an agent's pane is in now, if it says one.
pub fn now_in(agent: &Agent) -> Option<&str> {
    agent.zellij.as_ref().and_then(|zellij| zellij.session.as_deref())
}

/// A status the daemon knows, as it lists it.
fn known(status: Status) -> Reported {
    Reported::KnownStatus(status.into())
}

/// A file stands for an agent only while a process with its pid runs and
/// started when the file says: a file outlives the process that crashed, a pid
/// comes back as another process's, and a container that shares the folder
/// writes pids of a namespace of its own. The pid is the one the file is named
/// for, so a copy of a file is no second agent. The pane is the one the
/// process's environment names, and an agent outside zellij has none.
#[test]
fn the_daemon_lists_the_agents_whose_files_name_a_live_process() {
    let host = Host::new();
    let inside = process(Some(("work", 3)));
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
            (agent.session_id.as_str(), agent.pid, status, agent.zellij.clone())
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("inside", inside.pid(), Some(known(Status::Busy)), Some(zellij("work", None, 3))),
            ("outside", outside.pid(), Some(known(Status::Idle)), None),
        ]
    );
    let name = listed[0].name.as_ref().unwrap();
    assert_eq!((name.text.as_str(), name.source()), ("inside-name", NameSource::Derived));
    let entrypoint = listed[0].entrypoint.as_deref();
    assert_eq!((listed[0].cwd.as_str(), entrypoint), ("/home/you/work", Some("cli")));
}

/// What Claude Code says in words the daemon knows, it lists as those; a word
/// it does not know comes through as Claude Code wrote it, and what Claude
/// Code does not say, the daemon does not either: a status it does not date
/// has no date, though the file says when the session started.
#[test]
fn the_daemon_says_what_claude_code_says_and_no_more() {
    let host = Host::new();
    let (shell, newer, older) = (process(None), process(None), process(None));
    let mut said = file_for("shell", &shell, "shell");
    said["nameSource"] = json!("user");
    said["entrypoint"] = json!("sdk-cli");
    host.write(shell.pid(), &said);
    let mut said = file_for("newer", &newer, "Compacting");
    said["nameSource"] = json!("someone");
    said["entrypoint"] = json!("claude-vscode");
    host.write(newer.pid(), &said);
    let mut said = file_for("older", &older, "");
    for unsaid in ["status", "statusUpdatedAt", "nameSource", "entrypoint"] {
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
            (id, agent.status.clone(), dated, source, agent.entrypoint.as_deref())
        })
        .collect();
    let other = |word: &str| Some(Reported::OtherStatus(word.to_owned()));
    assert_eq!(
        seen,
        [
            ("newer", other("Compacting"), true, Some(NameSource::Other), Some("claude-vscode")),
            ("older", None, false, None, None),
            ("shell", Some(known(Status::Shell)), true, Some(NameSource::User), Some("sdk-cli")),
        ]
    );
}

/// The daemon follows the files as Claude Code writes them, with no one asking
/// it to look. A file read in the middle of a write says what it said before,
/// so that its agent does not blink out of the list. A file taken away takes
/// its agent, and so does a process that crashes and leaves its file.
#[test]
fn the_daemon_follows_the_files_as_they_change() {
    let host = Host::new();
    let (one, two) = (process(None), process(None));
    let (one_start, two_start) = (started(one.pid()), started(two.pid()));
    host.write(one.pid(), &file("one", one.pid(), &one_start, "busy"));
    host.write(two.pid(), &file("two", two.pid(), &two_start, "idle"));
    let _daemon = start(host.vars()).unwrap();

    host.write(one.pid(), &file("one", one.pid(), &one_start, "waiting"));
    let waits = Some(known(Status::Waiting));
    eventually("one waits", || host.agent("one").filter(|agent| agent.status == waits));

    // `two`'s new status shows once the daemon has read the folder again, and
    // that read finds `one` half written.
    let whole = file("one", one.pid(), &one_start, "idle").to_string();
    fs::write(host.file_of(one.pid()), &whole[..whole.len() / 2]).unwrap();
    host.write(two.pid(), &file("two", two.pid(), &two_start, "busy"));
    let works = Some(known(Status::Busy));
    eventually("two works", || host.agent("two").filter(|agent| agent.status == works));
    assert_eq!(host.agent("one").map(|agent| agent.status), Some(waits));

    fs::remove_file(host.file_of(one.pid())).unwrap();
    eventually("one is gone", || host.agent("one").is_none().then_some(()));

    let left = host.file_of(two.pid());
    drop(two);
    assert!(left.exists());
    eventually("two is gone", || host.agent("two").is_none().then_some(()));
}

/// A file that does not read for longer than a write takes is cut short for
/// good, or in a shape of a newer Claude Code's, or no session file at all: a
/// folder by a session file's name, one too long to be one, or one that
/// names no session or no directory. Its agent leaves the list, rather than
/// stay in it as it was, and the daemon counts the file as one it cannot
/// read, so that an empty list is not taken for no agents. It counts it while
/// a process runs by the file's pid, and not once that one has crashed and
/// left the file behind.
#[test]
fn a_file_the_daemon_cannot_read_is_counted_and_not_kept() {
    let host = Host::new();
    let (cut, newer, folder, long) = (process(None), process(None), process(None), process(None));
    let (unnamed, nowhere) = (process(None), process(None));
    host.write(cut.pid(), &file_for("cut", &cut, "busy"));
    let _daemon = start(host.vars()).unwrap();
    assert!(host.agent("cut").is_some());

    let whole = file_for("cut", &cut, "idle").to_string();
    fs::write(host.file_of(cut.pid()), &whole[..whole.len() / 2]).unwrap();
    let mut shape = file_for("newer", &newer, "busy");
    shape["procStart"] = json!({ "ticks": started(newer.pid()) });
    host.write(newer.pid(), &shape);
    fs::create_dir(host.file_of(folder.pid())).unwrap();
    // JSON a session file's parser would take, but for the spaces ahead of it.
    let padded = " ".repeat(64 * 1024) + &file_for("long", &long, "busy").to_string();
    fs::write(host.file_of(long.pid()), padded).unwrap();
    host.write(unnamed.pid(), &file_for("", &unnamed, "busy"));
    let mut blank = file_for("nowhere", &nowhere, "busy");
    blank["cwd"] = json!("");
    host.write(nowhere.pid(), &blank);
    let unread = eventually("six files are unread", || {
        let listed = host.list();
        (listed.unread == 6).then_some(listed)
    });
    assert!(unread.agents.is_empty(), "{:?}", unread.agents);

    let output = host.vars().command(LUNETA).arg("agents").output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("cannot read 6 of Claude Code's session files"), "{said}");

    let left = host.file_of(newer.pid());
    drop(newer);
    assert!(left.exists());
    eventually("five files are unread", || (host.list().unread == 5).then_some(()));
}

/// A file that does not read as the daemon starts is counted in its first
/// answer, as what the daemon knows is whole before it answers anyone. Each
/// is listed by where its process runs: in the session its environment
/// names, which the daemon follows through a rename as it does an agent's, by
/// when the process started, or outside zellij.
#[test]
fn a_file_that_does_not_read_is_counted_at_once_where_its_process_runs() {
    let host = Host::new();
    let work = host.contract().join("work");
    let _server = UnixListener::bind(&work).unwrap();
    let (inside, outside) = (process(Some(("work", 3))), process(None));
    for running in [&inside, &outside] {
        let mut shape = file_for("newer", running, "busy");
        shape["procStart"] = json!({ "ticks": started(running.pid()) });
        host.write(running.pid(), &shape);
    }
    let _daemon = start(host.vars()).unwrap();
    let listed = host.list();
    let in_work = UnreadFile { pid: inside.pid(), zellij: Some(zellij("work", Some("work"), 3)) };
    let mut files = vec![in_work, UnreadFile { pid: outside.pid(), zellij: None }];
    files.sort_by_key(|file| file.pid);
    assert_eq!((listed.unread, listed.unread_files), (2, files));

    fs::rename(&work, host.contract().join("play")).unwrap();
    let moved = eventually("the file's process is in play", || {
        let files = host.list().unread_files;
        let file = files.into_iter().find(|file| file.pid == inside.pid())?;
        file.zellij.filter(|zellij| zellij.session.as_deref() == Some("play"))
    });
    assert_eq!(moved, zellij("work", Some("play"), 3));
}

/// A folder that never stops changing is read anyway: the daemon waits for a
/// burst of changes to settle before it reads, but only so long. The test
/// writes a file beside the session files every few milliseconds, more often
/// than the folder ever settles, and meanwhile changes an agent's status.
#[test]
fn a_folder_that_never_settles_is_read_anyway() {
    let host = Host::new();
    let one = process(None);
    host.write(one.pid(), &file_for("one", &one, "busy"));
    let _daemon = start(host.vars()).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let churn = {
        let (stop, path) = (Arc::clone(&stop), host.sessions().join("churn"));
        let deadline = Instant::now() + Duration::from_secs(15);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                fs::write(&path, "churn").unwrap();
                thread::sleep(Duration::from_millis(5));
            }
        })
    };
    host.write(one.pid(), &file_for("one", &one, "idle"));
    let idle = Some(known(Status::Idle));
    eventually("one is idle", || host.agent("one").filter(|agent| agent.status == idle));
    stop.store(true, Ordering::Relaxed);
    churn.join().unwrap();
}

/// A folder the daemon cannot read, it says it cannot, and says so once: it
/// reads the folder again every sweep, and a line for each would bury the
/// rest of what it says. The test waits out more than two sweeps.
#[test]
fn a_folder_the_daemon_cannot_read_it_says_so_once() {
    let host = Host::new();
    fs::set_permissions(host.sessions(), Permissions::from_mode(0o000)).unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let said = daemon.said_after(Duration::from_secs(5)).to_vec();
    fs::set_permissions(host.sessions(), Permissions::from_mode(0o755)).unwrap();

    let sessions = host.sessions().display().to_string();
    let about: Vec<&String> = said.iter().filter(|line| line.contains(&sessions)).collect();
    assert!(about.iter().any(|line| line.ends_with("Permission denied (os error 13)")), "{said:?}");
    let once: HashSet<&&String> = about.iter().collect();
    assert_eq!(once.len(), about.len(), "{said:?}");
    assert!(host.agents().is_empty());
}

/// zellij renames a session by moving its socket to the new name. The daemon
/// sees it, and an agent whose environment still names the old session is in
/// the renamed one.
#[test]
fn a_renamed_session_takes_its_agents_with_it() {
    let host = Host::new();
    let _server = UnixListener::bind(host.contract().join("work")).unwrap();
    let agent = process(Some(("work", 2)));
    host.write(agent.pid(), &file_for("agent", &agent, "idle"));
    let _daemon = start(host.vars()).unwrap();
    assert_eq!(host.agent("agent").unwrap().zellij, Some(zellij("work", Some("work"), 2)));

    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    let moved = eventually("the agent is in play", || {
        host.agent("agent").filter(|agent| now_in(agent) == Some("play"))
    });
    assert_eq!(moved.zellij, Some(zellij("work", Some("play"), 2)));
}

/// A session made under the name another was renamed from does not take that
/// one's agents: the agent started before it was made, and so its pane did
/// not open in it. An agent started after both, whose environment names the
/// name, could be in either, and the daemon does not guess which.
#[test]
fn a_session_that_takes_a_name_does_not_take_the_agents_it_had() {
    let host = Host::new();
    let _server = UnixListener::bind(host.contract().join("work")).unwrap();
    let older = process(Some(("work", 2)));
    host.write(older.pid(), &file_for("older", &older, "idle"));
    let _daemon = start(host.vars()).unwrap();

    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    let in_play = |agent: &Agent| now_in(agent) == Some("play");
    eventually("the agent is in play", || host.agent("older").filter(in_play));

    let _taken = UnixListener::bind(host.contract().join("work")).unwrap();
    let newer = process(Some(("work", 5)));
    host.write(newer.pid(), &file_for("newer", &newer, "idle"));
    let either = eventually("the newer agent could be in either", || {
        let untold = |agent: &Agent| agent.zellij.as_ref().is_some_and(|z| z.session.is_none());
        host.agent("newer").filter(untold)
    });
    assert_eq!(either.zellij, Some(zellij("work", None, 5)));
    assert_eq!(host.agent("older").unwrap().zellij, Some(zellij("work", Some("play"), 2)));
}

/// A rename from before the daemon started, it never saw: the name the agent's
/// environment holds does not lead to the session made under it since, which
/// is younger than the agent.
#[test]
fn a_name_taken_before_the_daemon_started_leads_nowhere() {
    let host = Host::new();
    let _server = UnixListener::bind(host.contract().join("work")).unwrap();
    let agent = process(Some(("work", 2)));
    let mut said = file_for("agent", &agent, "idle");
    // A second ago, so that the session made next is younger by more than the
    // millisecond Claude Code dates a start to.
    said["startedAt"] = json!(now() - 1_000);
    host.write(agent.pid(), &said);
    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    let _taken = UnixListener::bind(host.contract().join("work")).unwrap();
    let _daemon = start(host.vars()).unwrap();

    assert_eq!(host.agent("agent").unwrap().zellij, Some(zellij("work", None, 2)));
}
