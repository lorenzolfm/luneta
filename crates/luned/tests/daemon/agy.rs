use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Stdio};

use lune_waits::eventually;
use luned::api::agent::{Status as Reported, WaitingFor as Waits};
use luned::api::{Agent, Harness, Status, TranscriptEndsRequest, WaitingFor};
use serde_json::{Value, json};

use crate::agents::{Host, MADE_NANOS, terminal};
use crate::host::{Daemon, PATIENT, Running, start};

const FAKE_AGY: &str = r#"
import { createInterface } from "node:readline";
import { execSync } from "node:child_process";

const [, cmd] = process.argv;
for await (const line of createInterface({ input: process.stdin })) {
  const out = execSync(cmd, { input: line, encoding: "utf8" });
  console.log(JSON.stringify(out));
}
"#;

fn script(host: &Host) -> PathBuf {
    host.agy_folder().join("lune-title.sh")
}

fn settings(host: &Host) -> PathBuf {
    host.agy_folder().join("settings.json")
}

fn title_command(host: &Host) -> Option<String> {
    let text = fs::read_to_string(settings(host)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value["title"]["command"].as_str().map(str::to_owned)
}

fn with_agy() -> (Host, Daemon) {
    let host = Host::new();
    fs::create_dir_all(host.agy_folder()).unwrap();
    let daemon = start(host.vars()).unwrap();
    eventually("the agy title hook is wired", || {
        (script(&host).is_file() && title_command(&host).is_some()).then_some(())
    });
    (host, daemon)
}

struct Agy {
    process: Running,
    events: ChildStdin,
    told: BufReader<ChildStdout>,
}

impl Agy {
    fn start(host: &Host, cmd: &str) -> Self {
        let mut command = host.vars().command("node");
        command
            .args(["--input-type=module", "--eval", FAKE_AGY])
            .arg(cmd)
            .env("LUNE_MUX_TERMINAL", "4")
            .env("LUNE_MUX_TERMINAL_MADE", MADE_NANOS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut process = Running::spawn(&mut command);
        let (events, told) = process.piped();
        Agy {
            process,
            events,
            told: BufReader::new(told),
        }
    }

    fn sends(&mut self, payload: &Value) -> String {
        writeln!(self.events, "{payload}").unwrap();
        let mut line = String::new();
        self.told.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).expect("a JSON-encoded stdout string")
    }
}

fn as_status(host: &Host, pid: u32, status: Status) -> Agent {
    eventually(&format!("the agy agent is {status:?}"), || {
        let mut agents = host.agents().into_iter();
        let listed = Some(Reported::KnownStatus(status.into()));
        agents.find(|agent| agent.pid == pid && agent.status == listed)
    })
}

#[test]
fn the_title_hook_is_installed_only_once_agy_s_folder_exists_and_chains_a_user_title() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    assert!(!host.agy_folder().exists());

    fs::create_dir_all(host.agy_folder()).unwrap();
    let user_settings = json!({
        "trustedWorkspaces": ["/w"],
        "title": {
            "type": "command",
            "command": "printf 'my-title'"
        }
    });
    fs::write(settings(&host), user_settings.to_string()).unwrap();

    let cmd = eventually("luned wires title in settings.json", || {
        title_command(&host).filter(|cmd| cmd.contains("lune-title.sh"))
    });
    let updated: Value =
        serde_json::from_str(&fs::read_to_string(settings(&host)).unwrap()).unwrap();
    assert_eq!(updated["trustedWorkspaces"], json!(["/w"]));

    let mut agy = Agy::start(&host, &cmd);
    let pid = agy.process.pid();
    let printed = agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "9c696c75-660f-4097-934f-e1cc290e4bd1",
        "conversation_title": "Feature parity",
        "agent_state": "idle"
    }));
    assert_eq!(printed, "my-title");

    let agent = as_status(&host, pid, Status::Idle);
    assert_eq!(agent.runs_in(), Harness::Antigravity);
    assert_eq!(
        agent.session_id.as_deref(),
        Some("9c696c75-660f-4097-934f-e1cc290e4bd1")
    );
    assert_eq!(agent.cwd, "/home/you/work");
    assert!(agent.at_a_prompt());
    assert_eq!(agent.terminal, Some(terminal(4)));
    assert_eq!(
        agent.name.as_ref().and_then(|name| name.given()),
        Some("Feature parity")
    );
}

#[test]
fn an_agy_agent_reports_status_waits_and_keeps_status_since_across_updates() {
    let (host, _daemon) = with_agy();
    let cmd = title_command(&host).unwrap();
    let mut agy = Agy::start(&host, &cmd);
    let pid = agy.process.pid();

    let printed = agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "",
        "agent_state": "idle"
    }));
    assert_eq!(printed, "");
    let empty = as_status(&host, pid, Status::Idle);
    assert_eq!(
        empty.session_id, None,
        "empty conversation leaves session_id absent"
    );

    agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "9c696c75-660f-4097-934f-e1cc290e4bd1",
        "agent_state": "working"
    }));
    let busy = as_status(&host, pid, Status::Busy);

    agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "9c696c75-660f-4097-934f-e1cc290e4bd1",
        "conversation_title": "Antigravity support",
        "agent_state": "tool_use"
    }));
    let still_busy = eventually("the agent gets its title", || {
        let agent = host.agents().into_iter().find(|agent| agent.pid == pid)?;
        agent
            .name
            .as_ref()
            .and_then(|name| name.given())
            .is_some()
            .then_some(agent)
    });
    assert_eq!(still_busy.status_since, busy.status_since);

    agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "9c696c75-660f-4097-934f-e1cc290e4bd1",
        "conversation_title": "Antigravity support",
        "agent_state": "working",
        "tool_confirmation_pending": true
    }));
    let waiting = as_status(&host, pid, Status::Waiting);
    assert_eq!(
        waiting.waiting_for,
        Some(Waits::KnownWaitingFor(WaitingFor::PermissionPrompt.into()))
    );

    agy.sends(&json!({
        "cwd": "/home/you/work",
        "conversation_id": "9c696c75-660f-4097-934f-e1cc290e4bd1",
        "conversation_title": "Antigravity support",
        "agent_state": "reviewing"
    }));
    let reviewing = eventually("the agent waits on input", || {
        let agent = host.agents().into_iter().find(|agent| agent.pid == pid)?;
        let expected = Some(Waits::KnownWaitingFor(WaitingFor::InputNeeded.into()));
        (agent.waiting_for == expected).then_some(agent)
    });
    assert_eq!(
        reviewing.status,
        Some(Reported::KnownStatus(Status::Waiting.into()))
    );

    agy.process.kill();
    eventually("the agy agent leaves the list once killed", || {
        host.agents().is_empty().then_some(())
    });
}

#[test]
fn transcript_ends_returns_antigravity_transcripts() {
    let (host, _daemon) = with_agy();
    let id = "9c696c75-660f-4097-934f-e1cc290e4bd1";
    let path = host
        .agy_folder()
        .join("brain")
        .join(id)
        .join(".system_generated/logs/transcript.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let user = json!({
        "step_index": 0,
        "source": "USER_EXPLICIT",
        "type": "USER_INPUT",
        "content": "<USER_REQUEST>\nadd agy\n</USER_REQUEST>"
    });
    let reply = json!({
        "step_index": 1,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "content": "Added Antigravity support."
    });
    fs::write(&path, format!("{user}\n{reply}\n")).unwrap();

    let folder = crate::host::folder_in(host.runtime()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let request = TranscriptEndsRequest {
            on: None,
            ids: vec![id.to_owned()],
            share: 8192,
        };
        daemon.transcript_ends(request).await.unwrap().into_inner()
    });
    let end = response
        .ends
        .get(id)
        .expect("the antigravity transcript is returned");
    assert!(end.contains("Added Antigravity support."), "{end}");
    let typed = response
        .typed_ends
        .get(id)
        .expect("the typed antigravity transcript is returned");
    assert_eq!(typed.harness, i32::from(Harness::Antigravity));
    assert!(
        typed.lines.contains("Added Antigravity support."),
        "{}",
        typed.lines
    );
}
