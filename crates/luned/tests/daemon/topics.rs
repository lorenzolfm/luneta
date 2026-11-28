use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::Stdio;

use lune_waits::{eventually, Span};
use luned::api::{self, remote::Reached, ClientWaiter, Harness, WatchChangesRequest};
use luned::SWEEP;
use serde_json::{json, Value};

use crate::agents::{file_for, process, MADE_NANOS};
use crate::host::{folder_in, script, side_by_side, start, Host, Running, Slot, Waits, PATIENT};
use crate::remote::{hosts, FakeSsh};

const ID: &str = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";

const SWEEPS: Span = Span::of(SWEEP.saturating_mul(3));

fn claude_that_names_topics(host: &Host) {
    let told = told_file(host);
    let text = format!(
        r#"prompt=$(cat)
{{ echo "cwd=$(pwd -P)"; printf '%s\n' "$@"; echo stdin:; printf '%s\n' "$prompt"; echo end-of-call; }} >> '{told}'
case "$prompt" in
    *please-fail*) echo 'Not logged in'; exit 1 ;;
    *'now the tabs'*) echo 'Tab order' ;;
    *) echo '"Session folds."' ;;
esac
"#,
        told = told.display()
    );
    script(&host.bin().join("claude"), &text);
}

fn told_file(host: &Host) -> PathBuf {
    host.home.path().join("told")
}

fn calls(host: &Host) -> Vec<String> {
    let told = fs::read_to_string(told_file(host)).unwrap_or_default();
    told.split_inclusive("end-of-call\n").map(str::to_owned).collect()
}

fn agent_at_work(host: &Host) -> Running {
    let agent = process(Some(3));
    let sessions = host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = file_for(ID, &agent, "busy").to_string();
    fs::write(sessions.join(format!("{}.json", agent.pid())), file).unwrap();
    agent
}

fn prompted(host: &Host, words: &str) {
    let project = host.home.path().join(".claude/projects/-w");
    fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{ID}.jsonl"));
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{}", json!({"type": "user", "message": {"content": words}})).unwrap();
}

fn seen(host: &Host) -> api::Host {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    runtime.block_on(async {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        let asked = WatchChangesRequest { after: None, waiter: ClientWaiter::Window.into() };
        daemon.watch_changes(asked).await.unwrap().into_inner().read_host().unwrap().into_host()
    })
}

fn topic(agents: &[api::Agent]) -> Option<&str> {
    agents.iter().find(|agent| agent.session_id.as_deref() == Some(ID))?.topic.as_deref()
}

#[test]
fn the_topics_a_window_has_named_hold_side_by_side() {
    side_by_side(const {
        Waits::of([
            ("this daemon names the topic", a_window_has_the_daemon_name_the_topic_on_each_new_prompt),
            (
                "another machine names its agents' topics",
                a_window_here_has_another_machine_name_its_agents_topics,
            ),
            (
                "this daemon names an agy agent's topic",
                a_window_has_the_daemon_name_an_agy_agent_s_topic_on_each_new_prompt,
            ),
        ])
    });
}

fn a_window_has_the_daemon_name_the_topic_on_each_new_prompt(_: Slot) {
    let host = Host::new();
    claude_that_names_topics(&host);
    let _daemon = start(host.vars()).unwrap();
    let _agent = agent_at_work(&host);
    prompted(&host, "fold the sessions");

    lune_waits::through(SWEEPS, "a topic is named with no window to show it", || {
        topic(&host.host().agents).map(str::to_owned)
    });
    assert_eq!(calls(&host), Vec::<String>::new());

    eventually("the topic is named", || {
        (topic(&seen(&host).agents) == Some("Session folds")).then_some(())
    });
    let called = calls(&host);
    let [call] = called.as_slice() else {
        panic!("{called:?}");
    };
    let home = fs::canonicalize(host.home.path()).unwrap();
    assert!(call.starts_with(&format!("cwd={}\n", home.display())), "{call}");
    let flags = ["-p", "--model\nhaiku", "--tools\n", "--safe-mode", "--no-session-persistence"];
    for flag in flags {
        assert!(call.contains(&format!("{flag}\n")), "{call}");
    }
    let (argv, stdin) = call.split_once("stdin:\n").unwrap();
    assert!(!argv.contains("fold the sessions"), "{call}");
    assert!(stdin.contains("The developer last asked it: fold the sessions"), "{call}");

    prompted(&host, "please-fail on this one");
    eventually("the call on it is made", || {
        seen(&host);
        (calls(&host).len() == 2).then_some(())
    });
    eventually("the log says why it failed", || {
        let log = host.home.path().join(".local/state/lune/lune.log");
        let log = fs::read_to_string(log).unwrap_or_default();
        log.contains("the model named no topic: claude: Not logged in").then_some(())
    });
    prompted(&host, "and now the tabs");
    eventually("the topic moves with the prompt", || {
        (topic(&seen(&host).agents) == Some("Tab order")).then_some(())
    });
    let called = calls(&host);
    assert_eq!(called.len(), 3, "{called:?}");
    assert!(called[1].contains("The topic the row goes by now: Session folds"), "{called:?}");
    assert!(called[2].contains("The topic the row goes by now: Session folds"), "{called:?}");

    assert_eq!(topic(&host.host().agents), Some("Tab order"));
}

fn a_window_here_has_another_machine_name_its_agents_topics(_: Slot) {
    let here = Host::new();
    let far = Host::new();
    claude_that_names_topics(&here);
    claude_that_names_topics(&far);
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    FakeSsh::new(far.bin()).reaches("here", here.vars());
    hosts(&here, "far\n");
    hosts(&far, "here\n");
    let _here = start(here.vars()).unwrap();
    let _far = start(far.vars()).unwrap();
    let _agent = agent_at_work(&far);
    prompted(&far, "fold the sessions");

    let far_agents = |host: &api::Host| {
        let remote = host.remotes.iter().find(|remote| remote.destination == "far");
        match remote.and_then(|remote| remote.reached.as_ref()) {
            Some(Reached::Host(far)) => far.agents.clone(),
            _ => Vec::new(),
        }
    };
    lune_waits::through(SWEEPS, "a topic is named with no window to show it", || {
        topic(&far_agents(&here.host())).map(str::to_owned)
    });
    assert_eq!(calls(&far), Vec::<String>::new());

    eventually("the topic comes through this daemon", || {
        (topic(&far_agents(&seen(&here))) == Some("Session folds")).then_some(())
    });
    assert_eq!(calls(&far).len(), 1);
    assert_eq!(calls(&here), Vec::<String>::new(), "each machine pays for its own");

    let mut prompts = 0;
    eventually("two daemons stop keeping each other shown", || {
        here.host();
        far.host();
        let before = calls(&far).len();
        prompts += 1;
        prompted(&far, &format!("prompt {prompts}"));
        let called = lune_waits::seen_within(SWEEPS, || (calls(&far).len() != before).then_some(()));
        called.is_none().then_some(())
    });
}

const FAKE_AGY: &str = r#"
import { createInterface } from "node:readline";
import { execSync } from "node:child_process";

const [, cmd] = process.argv;
for await (const line of createInterface({ input: process.stdin })) {
  const out = execSync(cmd, { input: line, encoding: "utf8" });
  console.log(JSON.stringify(out));
}
"#;

fn agy_prompted(host: &Host, step: usize, words: &str) {
    let path = host
        .home
        .path()
        .join(".gemini/antigravity-cli/brain")
        .join(ID)
        .join(".system_generated/logs/transcript.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    let record = json!({
        "step_index": step,
        "source": "USER_EXPLICIT",
        "type": "USER_INPUT",
        "content": format!("<USER_REQUEST>\n{words}\n</USER_REQUEST>")
    });
    writeln!(file, "{record}").unwrap();
}

fn a_window_has_the_daemon_name_an_agy_agent_s_topic_on_each_new_prompt(_: Slot) {
    let host = Host::new();
    claude_that_names_topics(&host);
    let agy_dir = host.home.path().join(".gemini/antigravity-cli");
    fs::create_dir_all(&agy_dir).unwrap();
    assert!(!host.home.path().join(".claude").exists());
    let _daemon = start(host.vars()).unwrap();

    let cmd = eventually("luned wires agy's title hook", || {
        let text = fs::read_to_string(agy_dir.join("settings.json")).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value["title"]["command"].as_str().map(str::to_owned)
    });

    let mut command = host.vars().command("node");
    command
        .args(["--input-type=module", "--eval", FAKE_AGY])
        .arg(&cmd)
        .env("LUNE_MUX_TERMINAL", "4")
        .env("LUNE_MUX_TERMINAL_MADE", MADE_NANOS.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut running = Running::spawn(&mut command);
    let (mut events, told) = running.piped();
    let mut told = BufReader::new(told);
    let mut send = |payload: &Value| {
        writeln!(events, "{payload}").unwrap();
        let mut line = String::new();
        told.read_line(&mut line).unwrap();
    };
    send(&json!({
        "cwd": "/home/you/work",
        "conversation_id": ID,
        "conversation_title": "Initial agy title",
        "agent_state": "working"
    }));
    eventually("the agy agent appears", || {
        host.host().agents.into_iter().find(|agent| {
            agent.session_id.as_deref() == Some(ID) && agent.runs_in() == Harness::Antigravity
        })
    });
    agy_prompted(&host, 0, "fold the sessions");

    lune_waits::through(SWEEPS, "no topic is named with no window to show it", || {
        topic(&host.host().agents).map(str::to_owned)
    });
    assert_eq!(calls(&host), Vec::<String>::new());

    eventually("the agy topic is named", || {
        (topic(&seen(&host).agents) == Some("Session folds")).then_some(())
    });
    let called = calls(&host);
    let [first] = called.as_slice() else {
        panic!("{called:?}");
    };
    assert!(first.contains("The topic the row goes by now: Initial agy title"), "{first}");
    assert!(first.contains("The developer last asked it: fold the sessions"), "{first}");

    agy_prompted(&host, 1, "and now the tabs");
    eventually("the agy topic moves with the next prompt", || {
        (topic(&seen(&host).agents) == Some("Tab order")).then_some(())
    });
    let called = calls(&host);
    assert_eq!(called.len(), 2, "{called:?}");
    assert!(called[1].contains("The topic the row goes by now: Session folds"), "{called:?}");
    assert_eq!(topic(&host.host().agents), Some("Tab order"));
}
