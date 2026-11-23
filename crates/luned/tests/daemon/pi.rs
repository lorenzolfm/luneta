use std::fs::{self, FileTimes, Permissions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::time::SystemTime;

use lune_waits::eventually;
use luned::api::agent::Status as Reported;
use luned::api::{Agent, Harness, Status};
use serde_json::{json, Value};

use crate::agents::{hidden, process, terminal, Host, MADE_NANOS};
use crate::host::{start, start_as, Daemon, Running, Vars, LUNED};

const PI: &str = r#"
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const [, extension, mode] = process.argv;
const url = pathToFileURL(extension).href;
let handlers, loads = 0, session = "first";
async function load() {
  handlers = new Map();
  const { default: factory } = await import(`${url}?${loads++}`);
  factory({ on: (name, handler) => handlers.set(name, handler) });
}
await load();
const ctx = { cwd: "/home/you/work", mode, sessionManager: { getSessionId: () => session } };
for await (const line of createInterface({ input: process.stdin })) {
  const event = JSON.parse(line);
  if (event.load) {
    session = event.load;
    await load();
  } else {
    await handlers.get(event.type)?.(event, ctx);
  }
  console.log("told");
}
"#;

fn extension(host: &Host) -> PathBuf {
    host.pi_folder().join("extensions/lune.ts")
}

fn with_pi() -> (Host, Daemon) {
    let host = Host::new();
    fs::create_dir_all(host.pi_folder()).unwrap();
    let daemon = start(host.vars()).unwrap();
    eventually("the extension is there", || extension(&host).is_file().then_some(()));
    (host, daemon)
}

struct Pi {
    process: Running,
    events: ChildStdin,
    told: BufReader<ChildStdout>,
}

impl Pi {
    fn start(host: &Host, mode: &str) -> Self {
        Self::start_with(host, host.vars(), mode)
    }

    fn start_with(host: &Host, vars: Vars<'_>, mode: &str) -> Self {
        let mut command = vars.command("node");
        command
            .args(["--input-type=module", "--eval", PI])
            .arg(extension(host))
            .arg(mode)
            .env("LUNE_MUX_TERMINAL", "3")
            .env("LUNE_MUX_TERMINAL_MADE", MADE_NANOS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut process = Running::spawn(&mut command);
        let (events, told) = process.piped();
        Pi { process, events, told: BufReader::new(told) }
    }

    fn says(&mut self, event: &Value) {
        writeln!(self.events, "{event}").unwrap();
        let mut told = String::new();
        self.told.read_line(&mut told).unwrap();
        assert_eq!(told, "told\n", "the stand-in for pi ended");
    }

    fn event(&mut self, name: &str) {
        self.says(&json!({ "type": name }));
    }

}

fn as_status(host: &Host, pid: u32, status: Status) -> Agent {
    eventually(&format!("the agent is {status:?}"), || {
        let mut agents = host.agents().into_iter();
        let listed = Some(Reported::KnownStatus(status.into()));
        agents.find(|agent| agent.pid == pid && agent.status == listed)
    })
}

fn file_of(host: &Host, pid: u32) -> PathBuf {
    host.runtime().join(format!("lune/pi/{pid}.json"))
}

fn says(id: &str) -> Value {
    json!({ "sessionId": id, "cwd": "/w", "status": "idle", "statusSince": 1_000 })
}

fn backdate(path: &Path) {
    let times = FileTimes::new().set_modified(SystemTime::UNIX_EPOCH);
    fs::File::options().write(true).open(path).unwrap().set_times(times).unwrap();
}

fn status_in(path: &Path) -> String {
    let said: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    said["status"].as_str().unwrap().to_owned()
}

#[test]
fn the_extension_goes_where_pi_finds_it_once_pi_has_run() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    assert!(!host.pi_folder().exists());

    fs::create_dir_all(extension(&host).parent().unwrap()).unwrap();
    let older = "// an older lune's";
    fs::write(extension(&host), older).unwrap();
    eventually("the extension is written over", || {
        (fs::read_to_string(extension(&host)).unwrap() != older).then_some(())
    });
    let there: Vec<_> = fs::read_dir(extension(&host).parent().unwrap()).unwrap().collect();
    assert_eq!(there.len(), 1, "it leaves only itself: {there:?}");
}

#[test]
fn a_pi_in_a_shell_that_moved_tmpdir_is_listed() {
    let host = Host::new();
    fs::create_dir_all(host.pi_folder()).unwrap();
    let _daemon = start(host.vars()).unwrap();
    eventually("the extension is there", || extension(&host).is_file().then_some(()));

    let moved = tempfile::tempdir().unwrap();
    let shell = Vars { runtime: None, temp: Some(moved.path()), ..host.vars() };
    let mut pi = Pi::start_with(&host, shell, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    as_status(&host, pi.process.pid(), Status::Idle);
    assert!(!moved.path().join("lune").exists());
}

#[test]
fn a_folder_s_spelling_is_no_code_in_the_extension() {
    let host = Host::new();
    fs::create_dir_all(host.pi_folder()).unwrap();
    let root = host.runtime().join(r#"a"b\c"#);
    fs::create_dir(&root).unwrap();
    let _daemon = start(Vars { runtime: Some(&root), ..host.vars() }).unwrap();
    eventually("the extension is there", || extension(&host).is_file().then_some(()));

    let mut pi = Pi::start(&host, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    assert!(root.join(format!("lune/pi/{}.json", pi.process.pid())).is_file());
}

#[test]
fn a_daemon_takes_the_extension_back_from_one_that_took_it_and_went() {
    let (host, _daemon) = with_pi();
    let its_own = fs::read_to_string(extension(&host)).unwrap();
    let (elsewhere, its_home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (runtime, home) = (Some(elsewhere.path()), Some(its_home.path()));
    let mut other = Vars { runtime, home, ..host.vars() }.command(LUNED);
    other.env("PI_CODING_AGENT_DIR", host.pi_folder());
    let other = start_as(other).unwrap();
    eventually("the other daemon has the extension", || {
        (fs::read_to_string(extension(&host)).unwrap() != its_own).then_some(())
    });
    drop(other);

    eventually("the daemon has it back", || {
        (fs::read_to_string(extension(&host)).unwrap() == its_own).then_some(())
    });
    let mut pi = Pi::start(&host, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    as_status(&host, pi.process.pid(), Status::Idle);
}

#[test]
fn a_pi_agent_is_listed_by_what_its_extension_hears() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();

    let before = SystemTime::now();
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    let idle = as_status(&host, pid, Status::Idle);
    assert_eq!(idle.runs_in(), Harness::Pi);
    assert_eq!((idle.session_id.as_str(), idle.cwd.as_str()), ("first", "/home/you/work"));
    assert_eq!(idle.entrypoint.as_deref(), Some("tui"));
    assert!(idle.at_a_prompt());
    assert_eq!(idle.terminal, Some(terminal(3)));
    let since: SystemTime = idle.status_since.unwrap().try_into().unwrap();
    let millis = |at: SystemTime| at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis();
    assert!(millis(before) <= millis(since), "dated {since:?}, after {before:?}");
    assert!(idle.started_at.is_some());

    pi.event("agent_start");
    let busy = as_status(&host, pid, Status::Busy);
    assert_ne!(busy.status_since, idle.status_since);
    pi.event("ui_prompt_start");
    as_status(&host, pid, Status::Waiting);
    pi.event("ui_prompt_end");
    as_status(&host, pid, Status::Busy);
    pi.event("agent_settled");
    as_status(&host, pid, Status::Idle);
}

#[test]
fn a_prompt_at_rest_leaves_an_idle_agent_idle() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    let file = file_of(&host, pi.process.pid());
    let idle = fs::read(&file).unwrap();

    pi.event("ui_prompt_start");
    assert_eq!(fs::read(&file).unwrap(), idle);
    pi.event("ui_prompt_end");
    assert_eq!(fs::read(&file).unwrap(), idle);
}

#[test]
fn a_pi_agent_goes_on_through_a_new_session_and_goes_as_it_quits() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    let file = file_of(&host, pid);

    pi.says(&json!({ "type": "session_start", "reason": "reload" }));
    let first = as_status(&host, pid, Status::Idle);
    assert_eq!(first.session_id, "first");

    let said = fs::read(&file).unwrap();
    pi.says(&json!({ "type": "session_shutdown", "reason": "reload" }));
    pi.says(&json!({ "load": "first" }));
    assert_eq!(fs::read(&file).unwrap(), said, "it goes on as the extension loads");
    pi.says(&json!({ "type": "session_start", "reason": "reload" }));
    assert_eq!(fs::read(&file).unwrap(), said, "the reload said nothing");

    pi.says(&json!({ "type": "session_shutdown", "reason": "new" }));
    assert_eq!(fs::read(&file).unwrap(), said, "it goes on as the next session loads");
    pi.says(&json!({ "load": "second" }));
    pi.says(&json!({ "type": "session_start", "reason": "new" }));
    eventually("the agent is on its new session", || {
        let agents = host.agents();
        let on_second = matches!(agents.as_slice(), [agent] if agent.session_id == "second");
        on_second.then_some(())
    });

    pi.says(&json!({ "type": "session_shutdown", "reason": "quit" }));
    assert!(!file.exists());
    eventually("the agent has gone", || host.agents().is_empty().then_some(()));
    pi.event("agent_settled");
    assert!(!file.exists(), "a late event brings no file back");
}

#[test]
fn a_pi_agent_that_is_killed_leaves_the_list_as_it_ends() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    as_status(&host, pid, Status::Idle);

    pi.process.kill();
    eventually("the agent has gone", || host.agents().is_empty().then_some(()));
    assert!(file_of(&host, pid).exists(), "it left its file");
}

#[test]
fn a_pi_at_no_prompt_says_how_it_runs() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "print");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    let agent = as_status(&host, pi.process.pid(), Status::Idle);
    assert_eq!(agent.entrypoint.as_deref(), Some("print"));
}

#[test]
fn a_file_stands_only_for_the_process_that_wrote_it() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let folder = host.runtime().join("lune/pi");
    let file = |pid: u32| folder.join(format!("{pid}.json"));

    let killed = process(None);
    let killed_pid = killed.pid();
    drop(killed);
    fs::write(file(killed_pid), says("killed").to_string()).unwrap();

    let reused = process(None);
    fs::write(file(reused.pid()), says("reused").to_string()).unwrap();
    backdate(&file(reused.pid()));

    let contained = process(None);
    let mut of_another = says("contained");
    of_another["procStart"] = json!("1");
    fs::write(file(contained.pid()), of_another.to_string()).unwrap();

    let unread = [
        json!({ "cwd": "/w", "status": "idle", "statusSince": 1_000 }),
        json!({ "sessionId": "no status", "cwd": "/w", "statusSince": 1_000 }),
        json!({ "sessionId": "no date", "cwd": "/w", "status": "idle" }),
        json!({ "sessionId": "relative", "cwd": "w", "status": "idle", "statusSince": 1_000 }),
    ];
    let _unread: Vec<Running> = unread
        .iter()
        .map(|said| {
            let running = process(None);
            fs::write(file(running.pid()), said.to_string()).unwrap();
            running
        })
        .collect();

    let copied = process(None);
    let copy = folder.join(format!("{}-copy.json", copied.pid()));
    fs::write(copy, says("copied").to_string()).unwrap();

    let running = process(None);
    fs::write(file(running.pid()), says("running").to_string()).unwrap();
    let found = eventually("the agent is listed", || {
        let list = host.list();
        list.agents.iter().any(|agent| agent.session_id == "running").then_some(list)
    });
    let ids: Vec<&str> = found.agents.iter().map(|agent| agent.session_id.as_str()).collect();
    assert_eq!(ids, ["running"]);
    assert_eq!(found.unread, 0);
}

#[test]
fn a_file_no_process_of_its_pid_wrote_says_nothing_of_that_process_s_terminal() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let hidden = hidden();
    let pid = hidden.pid;
    let file = host.runtime().join(format!("lune/pi/{pid}.json"));
    let mut of_another = says("of another");
    of_another["procStart"] = json!("1");
    fs::write(&file, of_another.to_string()).unwrap();

    let unwritten = format!("{}: no process of its pid wrote it", file.display());
    let log = eventually("its file is found", || {
        let log = host.logged();
        log.contains(&unwritten).then_some(log)
    });
    assert!(!log.contains(&format!("agent {pid} shows in no view")), "{log}");
}

#[test]
fn a_file_that_names_its_process_s_start_stands_however_old_it_looks() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    backdate(&file_of(&host, pid));

    let running = process(None);
    fs::write(file_of(&host, running.pid()), says("running").to_string()).unwrap();
    let agents = eventually("the other agent is listed", || {
        let agents = host.agents();
        agents.iter().any(|agent| agent.session_id == "running").then_some(agents)
    });
    assert_eq!(agents.iter().any(|agent| agent.pid == pid), cfg!(target_os = "linux"));
}

#[test]
fn a_status_that_could_not_be_written_is_written_when_it_next_comes() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    let file = file_of(&host, pi.process.pid());
    let folder = file.parent().unwrap();

    fs::set_permissions(folder, Permissions::from_mode(0o500)).unwrap();
    pi.event("agent_start");
    fs::set_permissions(folder, Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status_in(&file), "idle");

    pi.event("agent_start");
    assert_eq!(status_in(&file), "busy");
}

#[test]
fn a_turn_goes_on_through_a_reload() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "rpc");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    pi.event("agent_start");

    pi.says(&json!({ "type": "session_shutdown", "reason": "reload" }));
    pi.says(&json!({ "load": "first" }));
    pi.says(&json!({ "type": "session_start", "reason": "reload" }));
    let file = file_of(&host, pi.process.pid());
    assert_eq!(status_in(&file), "busy");

    pi.event("ui_prompt_start");
    assert_eq!(status_in(&file), "waiting");
}
