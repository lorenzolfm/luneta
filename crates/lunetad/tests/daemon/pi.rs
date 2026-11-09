//! The agents of pi, which keeps no file of them: luneta's extension does, and
//! the daemon puts it where pi finds it and lists what it says. `node` runs the
//! extension as the daemon put it, as pi would, so that what it writes is what
//! the daemon reads.

use std::fs::{self, FileTimes, Permissions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Harness, Status};
use serde_json::{json, Value};

use crate::agents::{process, Host};
use crate::host::{eventually, start, Daemon, Running, Vars};

/// Stands in for pi: loads the extension, and tells it of each event on a line
/// of stdin, with the context pi gives it. `load` has it load the extension
/// anew, on the session `load` names, as pi does in replacing a session.
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

/// The extension's place, in the folder pi finds extensions in.
fn extension(host: &Host) -> PathBuf {
    host.pi_folder().join("extensions/luneta.ts")
}

/// A host where pi has run, with a daemon that has put the extension there.
fn with_pi() -> (Host, Daemon) {
    let host = Host::new();
    fs::create_dir_all(host.pi_folder()).unwrap();
    let daemon = start(host.vars()).unwrap();
    eventually("the extension is there", || extension(&host).is_file().then_some(()));
    (host, daemon)
}
/// A process of pi's, in the pane `work:3`, in the mode `mode`, as pi names it.
struct Pi {
    process: Running,
    events: ChildStdin,
    told: BufReader<ChildStdout>,
}

impl Pi {
    fn start(host: &Host, mode: &str) -> Self {
        Self::start_with(host, host.vars(), mode)
    }

    /// One started with the variables `vars`, as a shell of its own has them.
    fn start_with(host: &Host, vars: Vars<'_>, mode: &str) -> Self {
        let mut command = vars.command("node");
        command
            .args(["--input-type=module", "--eval", PI])
            .arg(extension(host))
            .arg(mode)
            .env("ZELLIJ_SESSION_NAME", "work")
            .env("ZELLIJ_PANE_ID", "3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut process = Running::spawn(&mut command);
        let (events, told) = process.piped();
        Pi { process, events, told: BufReader::new(told) }
    }

    /// Has it take `event`, and waits until it has.
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

/// The agent the daemon lists for the pi of `pid`, once it is `status`.
fn as_status(host: &Host, pid: u32, status: Status) -> Agent {
    eventually(&format!("the agent is {status:?}"), || {
        let mut agents = host.agents().into_iter();
        let listed = Some(Reported::KnownStatus(status.into()));
        agents.find(|agent| agent.pid == pid && agent.status == listed)
    })
}

/// The file the extension keeps for the pi of `pid`. It has written it by the
/// time it has taken an event, so that what it did not write, this tells at
/// once, as the daemon's list would only in a moment.
fn file_of(host: &Host, pid: u32) -> PathBuf {
    host.runtime().join(format!("luneta/pi/{pid}.json"))
}

/// What a file of the extension holds, as one that cannot tell its process's
/// start writes it.
fn says(id: &str) -> Value {
    json!({ "sessionId": id, "cwd": "/w", "status": "idle", "statusSince": 1_000 })
}

/// Dates the file at `path` an hour back.
fn backdate(path: &Path) {
    let long_ago = SystemTime::now() - Duration::from_hours(1);
    let times = FileTimes::new().set_modified(long_ago);
    fs::File::options().write(true).open(path).unwrap().set_times(times).unwrap();
}

/// What the file at `path` says the agent is doing.
fn status_in(path: &Path) -> String {
    let said: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    said["status"].as_str().unwrap().to_owned()
}

/// The daemon puts the extension where pi finds it once pi has run on the
/// machine, as it can have long after the daemon started, and nothing before
/// that: a machine with no pi gets no folder of pi's. What is there under its
/// name is written over, as an older luneta's is.
#[test]
fn the_extension_goes_where_pi_finds_it_once_pi_has_run() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    assert!(!host.pi_folder().exists());

    fs::create_dir_all(extension(&host).parent().unwrap()).unwrap();
    let older = "// an older luneta's";
    fs::write(extension(&host), older).unwrap();
    eventually("the extension is written over", || {
        (fs::read_to_string(extension(&host)).unwrap() != older).then_some(())
    });
    let there: Vec<_> = fs::read_dir(extension(&host).parent().unwrap()).unwrap().collect();
    assert_eq!(there.len(), 1, "it leaves only itself: {there:?}");
}

/// A launcher of pi's that keeps a folder of its own, as numes does with its
/// `PI_CODING_AGENT_DIR`, has the extension of its own too: an agent started
/// there reads none of the one in pi's own folder, and would tell a folder no
/// daemon reads.
#[test]
fn a_launcher_s_folder_of_pi_s_gets_the_extension_of_its_own() {
    let host = Host::new();
    fs::create_dir_all(host.numes_pi_folder()).unwrap();
    let _daemon = start(host.vars()).unwrap();
    let numes = host.numes_pi_folder().join("extensions/luneta.ts");
    eventually("the extension is in numes's folder", || numes.is_file().then_some(()));
    let told = fs::read_to_string(&numes).unwrap();
    assert_eq!(
        told.lines().find(|line| line.contains("const FOLDER")),
        Some(format!("const FOLDER = {:?};", host.runtime().join("luneta")).as_str()),
        "it tells the daemon's folder"
    );
    // pi's own folder stays where it was: none, where pi has not run.
    assert!(!host.pi_folder().exists());
}

/// Where pi starts, the variables that name luneta's folder can say another:
/// `nix develop` moves `TMPDIR`, which names it where there is no
/// `XDG_RUNTIME_DIR`, as on macOS. The agent is listed all the same, and nothing
/// is left where no daemon reads.
#[test]
fn a_pi_in_a_shell_that_moved_tmpdir_is_listed() {
    let host = Host::new();
    fs::create_dir_all(host.pi_folder()).unwrap();
    let _daemon = start(Vars { runtime: None, temp: Some(host.runtime()), ..host.vars() }).unwrap();
    eventually("the extension is there", || extension(&host).is_file().then_some(()));

    let moved = tempfile::tempdir().unwrap();
    let shell = Vars { runtime: None, temp: Some(moved.path()), ..host.vars() };
    let mut pi = Pi::start_with(&host, shell, "tui");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    as_status(&host, pi.process.pid(), Status::Idle);
    assert!(!moved.path().join("luneta").exists());
}

/// The folder goes into the extension's code as text, so what it is spelled
/// with is no code: a `"` does not end that text, nor a `\` make it mean another
/// folder.
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
    assert!(root.join(format!("luneta/pi/{}.json", pi.process.pid())).is_file());
}

/// A daemon that starts in another folder takes the extension over, as nothing
/// else would say that the one on disk names the wrong folder. Once it has gone,
/// the daemon that still runs takes it back, or each pi that starts after would
/// tell a folder no daemon reads, and get no row, with nothing said of it.
#[test]
fn a_daemon_takes_the_extension_back_from_one_that_took_it_and_went() {
    let (host, _daemon) = with_pi();
    let its_own = fs::read_to_string(extension(&host)).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let other = start(Vars { runtime: Some(elsewhere.path()), ..host.vars() }).unwrap();
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

/// What the extension hears of an agent is what the daemon lists: where it is
/// and works, by pi's own word, from the session's start; busy from a run's
/// start, waiting only for a prompt in a run, and idle once it settles, each
/// dated as it came; and which harness it is.
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
    let zellij = idle.zellij.clone().unwrap();
    assert_eq!((zellij.named.as_str(), zellij.pane), ("work", 3));
    let since: SystemTime = idle.status_since.unwrap().try_into().unwrap();
    assert!(before - Duration::from_millis(1) <= since, "dated {since:?}, after {before:?}");
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

/// A prompt at rest is one you opened, and asks nothing of an idle agent: its
/// status stands, and its date, so that it does not come to look newly done.
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

/// An agent goes on through what replaces its session: its file stands while
/// pi loads its extensions again, and then says the new session. A reload is
/// no turn, and leaves its status dated as it was, but for a pi that runs on
/// and first takes the extension by a reload. A quit takes the agent away.
#[test]
fn a_pi_agent_goes_on_through_a_new_session_and_goes_as_it_quits() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    let file = file_of(&host, pid);

    // A pi that was running as the extension came, which `/reload` gave it.
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

/// A pi that is killed says no word to the folder, and leaves its file. It
/// leaves the list as its process ends, and not at the next sweep, two seconds
/// off: it is killed just after the read that found it.
#[test]
fn a_pi_agent_that_is_killed_leaves_the_list_as_it_ends() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    as_status(&host, pid, Status::Idle);

    let killed = Instant::now();
    pi.process.kill();
    eventually("the agent has gone", || host.agents().is_empty().then_some(()));
    let took = killed.elapsed();
    assert!(took < Duration::from_secs(1), "it stayed listed for {took:?}");
    assert!(file_of(&host, pid).exists(), "it left its file");
}

/// A pi that is not the agent you talk to at a prompt says the mode it runs in,
/// so that a pane seats the one you do talk to.
#[test]
fn a_pi_at_no_prompt_says_how_it_runs() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "print");
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    let agent = as_status(&host, pi.process.pid(), Status::Idle);
    assert_eq!(agent.entrypoint.as_deref(), Some("print"));
}

/// A file stands for an agent only while the process that runs by its pid wrote
/// it. A killed pi leaves its file behind, and its pid comes back as another
/// process's, which started after the file was written. A pi in a container
/// that shares the folder names a start that is no process's here. A file that
/// does not read says nothing, nor counts among Claude Code's that do not, and
/// only a file named for a pid stands for one.
#[test]
fn a_file_stands_only_for_the_process_that_wrote_it() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let folder = host.runtime().join("luneta/pi");
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

    // No session, no status and no date, which the extension always writes.
    let unread = [
        json!({ "cwd": "/w", "status": "idle", "statusSince": 1_000 }),
        json!({ "sessionId": "no status", "cwd": "/w", "statusSince": 1_000 }),
        json!({ "sessionId": "no date", "cwd": "/w", "status": "idle" }),
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

    // The last written, so that the read that finds it has found the rest.
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

/// A clock set forward makes a process look newer than what it wrote, and its
/// file look left behind. On Linux the extension names its process's start,
/// which no clock moves, and the agent stays listed. On macOS it does not, and
/// there the date of a process is one that no clock moves either.
#[test]
fn a_file_that_names_its_process_s_start_stands_however_old_it_looks() {
    let (host, _daemon) = with_pi();
    let mut pi = Pi::start(&host, "tui");
    let pid = pi.process.pid();
    pi.says(&json!({ "type": "session_start", "reason": "startup" }));
    backdate(&file_of(&host, pid));

    // The last written, so that the read that finds it has found the rest.
    let running = process(None);
    fs::write(file_of(&host, running.pid()), says("running").to_string()).unwrap();
    let agents = eventually("the other agent is listed", || {
        let agents = host.agents();
        agents.iter().any(|agent| agent.session_id == "running").then_some(agents)
    });
    assert_eq!(agents.iter().any(|agent| agent.pid == pid), cfg!(target_os = "linux"));
}

/// A status that could not be written is written when it next comes, as a retry
/// brings `agent_start` again: the file is not taken to say it meanwhile.
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

/// A reload in a turn, as an extension's command in RPC mode can ask for, goes
/// on with the turn, by what the file says: a prompt in the rest of it counts.
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
