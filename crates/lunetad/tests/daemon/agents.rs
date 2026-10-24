//! The agents the daemon finds: Claude Code's session files, the processes
//! they name, and the zellij panes those run in.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;

use lunetad::api::{Agent, ListAgentsRequest, Status, Zellij};
use serde_json::json;
use tempfile::TempDir;

use crate::host::{eventually, start, Running, Vars};

/// The folders of a host of a test's own: the daemon's, a home with Claude
/// Code's session files in it, and zellij's sockets.
struct Host {
    runtime: TempDir,
    home: TempDir,
    zellij: TempDir,
}

impl Host {
    fn new() -> Self {
        let host = Host {
            runtime: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            zellij: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(host.sessions()).unwrap();
        fs::create_dir_all(host.contract()).unwrap();
        host
    }

    fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            home: Some(self.home.path()),
            zellij: Some(self.zellij.path()),
            ..Vars::default()
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.path().join(".claude/sessions")
    }

    /// The folder zellij 0.45 keeps its sessions' sockets in.
    fn contract(&self) -> PathBuf {
        self.zellij.path().join("contract_version_1")
    }

    /// Writes a session file as Claude Code does, for the agent `id`.
    fn write(&self, id: &str, file: &serde_json::Value) {
        fs::write(self.sessions().join(format!("{id}.json")), file.to_string()).unwrap();
    }

    /// What the daemon lists.
    fn agents(&self) -> Vec<Agent> {
        let socket = self.runtime.path().join(crate::host::SOCKET);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&socket).await.unwrap();
            daemon.list_agents(ListAgentsRequest {}).await.unwrap().into_inner().agents
        })
    }

    /// The agent the daemon lists for the session `id`.
    fn agent(&self, id: &str) -> Option<Agent> {
        self.agents().into_iter().find(|agent| agent.session_id == id)
    }
}

/// A process to stand for an agent, in the pane `pane` names, if any.
fn process(pane: Option<(&str, u32)>) -> Running {
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

/// A session file for the agent `id`, whose process is `pid` and started at
/// `start`.
fn file(id: &str, pid: u32, start: &str, status: &str) -> serde_json::Value {
    json!({
        "pid": pid,
        "sessionId": id,
        "cwd": "/home/you/work",
        "procStart": start,
        "status": status,
        "statusUpdatedAt": 1_792_859_293_927_u64,
        "startedAt": 1_792_856_818_459_u64,
        "name": format!("{id}-name"),
        "nameSource": "derived",
        "entrypoint": "cli",
        "kind": "interactive",
    })
}

fn zellij(named: &str, session: &str, pane: u32) -> Zellij {
    Zellij { named: named.to_owned(), session: session.to_owned(), pane }
}

/// A file stands for an agent only while a process with its pid runs and
/// started when the file says: a file outlives the process that crashed, a pid
/// comes back as another process's, and a container that shares the folder
/// writes pids of a namespace of its own. The pane is the one the process's
/// environment names, and an agent outside zellij has none.
#[test]
fn the_daemon_lists_the_agents_whose_files_name_a_live_process() {
    let host = Host::new();
    let inside = process(Some(("work", 3)));
    let outside = process(None);
    let crashed = process(None);
    let (crashed_pid, crashed_start) = (crashed.pid(), started(crashed.pid()));
    drop(crashed);

    host.write("inside", &file("inside", inside.pid(), &started(inside.pid()), "busy"));
    host.write("outside", &file("outside", outside.pid(), &started(outside.pid()), "idle"));
    host.write("crashed", &file("crashed", crashed_pid, &crashed_start, "busy"));
    host.write("reused", &file("reused", inside.pid(), "0", "busy"));
    let mut foreign = file("foreign", outside.pid(), &started(outside.pid()), "busy");
    foreign["pidDomain"] = json!("linux:elsewhere:pid:[1]");
    host.write("foreign", &foreign);
    let _daemon = start(host.vars()).unwrap();

    let mut listed = host.agents();
    listed.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    let seen: Vec<_> = listed
        .iter()
        .map(|agent| (agent.session_id.as_str(), agent.pid, agent.status(), agent.zellij.clone()))
        .collect();
    assert_eq!(
        seen,
        [
            ("inside", inside.pid(), Status::Busy, Some(zellij("work", "", 3))),
            ("outside", outside.pid(), Status::Idle, None),
        ]
    );
    let name = listed[0].name.as_ref().unwrap();
    assert_eq!((name.text.as_str(), name.source.as_str()), ("inside-name", "derived"));
    assert_eq!((listed[0].cwd.as_str(), listed[0].entrypoint.as_str()), ("/home/you/work", "cli"));
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
    host.write("one", &file("one", one.pid(), &one_start, "busy"));
    host.write("two", &file("two", two.pid(), &two_start, "idle"));
    let _daemon = start(host.vars()).unwrap();

    host.write("one", &file("one", one.pid(), &one_start, "waiting"));
    eventually("one waits", || host.agent("one").filter(|agent| agent.status() == Status::Waiting));

    // `two`'s new status shows once the daemon has read the folder again, and
    // that read finds `one` half written.
    let whole = file("one", one.pid(), &one_start, "idle").to_string();
    fs::write(host.sessions().join("one.json"), &whole[..whole.len() / 2]).unwrap();
    host.write("two", &file("two", two.pid(), &two_start, "busy"));
    eventually("two works", || host.agent("two").filter(|agent| agent.status() == Status::Busy));
    assert_eq!(host.agent("one").map(|agent| agent.status()), Some(Status::Waiting));

    fs::remove_file(host.sessions().join("one.json")).unwrap();
    eventually("one is gone", || host.agent("one").is_none().then_some(()));

    drop(two);
    assert!(host.sessions().join("two.json").exists());
    eventually("two is gone", || host.agent("two").is_none().then_some(()));
}

/// zellij renames a session by moving its socket to the new name. The daemon
/// sees it, and an agent whose environment still names the old session is in
/// the renamed one.
#[test]
fn a_renamed_session_takes_its_agents_with_it() {
    let host = Host::new();
    let _server = UnixListener::bind(host.contract().join("work")).unwrap();
    let agent = process(Some(("work", 2)));
    host.write("agent", &file("agent", agent.pid(), &started(agent.pid()), "idle"));
    let _daemon = start(host.vars()).unwrap();
    assert_eq!(host.agent("agent").unwrap().zellij, Some(zellij("work", "work", 2)));

    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    let moved = eventually("the agent is in play", || {
        host.agent("agent").filter(|agent| agent.zellij.as_ref().is_some_and(|z| z.session == "play"))
    });
    assert_eq!(moved.zellij, Some(zellij("work", "play", 2)));
}
