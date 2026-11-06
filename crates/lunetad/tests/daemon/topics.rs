//! The topics a daemon keeps of its machine's agents, as a window of luneta's
//! sees them, here and on another machine: what a `claude` of the test's own is
//! asked, and when, and what comes of it.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::{self, remote::Reached, WatchRequest};
use serde_json::json;

use crate::agents::{file_for, process};
use crate::host::{eventually, folder_in, script, start, Host, Running};
use crate::remote::{hosts, FakeSsh};

const ID: &str = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";

/// Long enough for a daemon with a prompt to name a topic on to have made its
/// call: two looks at its agents, a sweep apart.
const SWEEPS: Duration = Duration::from_secs(5);

/// Has `host`'s `claude` note each call, in `told`, and name a topic for the
/// words of the prompt it is given: it fails on `please-fail`.
fn claude_that_names_topics(host: &Host) {
    let told = told_file(host);
    let text = format!(
        r#"{{ echo "cwd=$(pwd -P)"; printf '%s\n' "$@"; echo end-of-call; }} >> '{told}'
case "$*" in
    *please-fail*) echo 'Not logged in' >&2; exit 1 ;;
    *'now the tabs'*) echo 'Tab order' ;;
    *) echo '"Session folds."' ;;
esac
"#,
        told = told.display()
    );
    script(&host.zellij.bin().join("claude"), &text);
}

fn told_file(host: &Host) -> PathBuf {
    host.home.path().join("told")
}

/// What `host`'s `claude` was told, a call each.
fn calls(host: &Host) -> Vec<String> {
    let told = fs::read_to_string(told_file(host)).unwrap_or_default();
    told.split_inclusive("end-of-call\n").map(str::to_owned).collect()
}

/// An agent of `host`'s, at work in a pane, from after its daemon started.
fn agent_at_work(host: &Host) -> Running {
    let agent = process(Some(("work", 3)));
    let sessions = host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = file_for(ID, &agent, "busy").to_string();
    fs::write(sessions.join(format!("{}.json", agent.pid())), file).unwrap();
    agent
}

/// Has `host`'s agent asked `words`, as Claude Code records a prompt.
fn prompted(host: &Host, words: &str) {
    let project = host.home.path().join(".claude/projects/-w");
    fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{ID}.jsonl"));
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{}", json!({"type": "user", "message": {"content": words}})).unwrap();
}

/// What `host`'s daemon answers a window of luneta's, which shows it.
fn seen(host: &Host) -> api::Host {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let asked = WatchRequest { after: None, kept: None, shown: true };
        daemon.watch(asked).await.unwrap().into_inner()
    })
}

/// The topic of the agent `ID` in `agents`, where it has one.
fn topic(agents: &[api::Agent]) -> Option<&str> {
    agents.iter().find(|agent| agent.session_id == ID)?.topic.as_deref()
}

/// A question asked once, as `luneta sessions` asks, shows no one anything, and
/// costs no call. A window does, whose rows go by the topic the model names on
/// each new prompt, without what it wraps one in: one call on each, in your
/// home, told what you asked. One that fails leaves the topic as it was, and
/// the next prompt asks again.
#[test]
fn a_window_has_the_daemon_name_the_topic_on_each_new_prompt() {
    let host = Host::new();
    claude_that_names_topics(&host);
    let _daemon = start(host.vars()).unwrap();
    let _agent = agent_at_work(&host);
    prompted(&host, "fold the sessions");

    let until = Instant::now() + SWEEPS;
    while Instant::now() < until {
        assert_eq!(topic(&host.host().agents), None);
        thread::sleep(Duration::from_millis(100));
    }
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
    assert!(call.contains("The developer last asked it: fold the sessions"), "{call}");

    prompted(&host, "please-fail on this one");
    eventually("the call on it is made", || {
        seen(&host);
        (calls(&host).len() == 2).then_some(())
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

/// The agents of another machine are named there, on that machine's `claude`,
/// for a window here, whose rows are told the topic through this daemon. A
/// daemon that watches that machine with no window of its own costs no call,
/// though it keeps that machine running, and though it and that machine watch
/// each other, as neither takes the other's word for a window it has none of:
/// a prompt once the window has gone costs none either.
#[test]
fn a_window_here_has_another_machine_name_its_agents_topics() {
    let here = Host::new();
    let far = Host::new();
    claude_that_names_topics(&here);
    claude_that_names_topics(&far);
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    FakeSsh::new(far.zellij.bin()).reaches("here", here.vars());
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
    let until = Instant::now() + SWEEPS;
    while Instant::now() < until {
        assert_eq!(topic(&far_agents(&here.host())), None);
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(calls(&far), Vec::<String>::new());

    eventually("the topic comes through this daemon", || {
        (topic(&far_agents(&seen(&here))) == Some("Session folds")).then_some(())
    });
    assert_eq!(calls(&far).len(), 1);
    assert_eq!(calls(&here), Vec::<String>::new(), "each machine pays for its own");

    // With no window, what the two say of each other soon says no one sees.
    let mut prompts = 0;
    let unseen = Instant::now() + Duration::from_mins(1);
    loop {
        assert!(Instant::now() < unseen, "two daemons keep each other shown");
        here.host();
        far.host();
        let before = calls(&far).len();
        prompts += 1;
        prompted(&far, &format!("prompt {prompts}"));
        thread::sleep(SWEEPS);
        if calls(&far).len() == before {
            break;
        }
    }
}
