use std::fs;
use std::path::Path;

use lune_protocol::agent_status::{AgentStatus, BlockedReason};
use lune_waits::eventually;
use luned::api::{Agent, Harness, StatusRung};

use crate::host::script;
use crate::own::Owned;

const THREAD: &str = "01a222f4-cde9-75d3-9267-061cca269c80";

const NEWER_THREAD: &str = "01a22300-2f28-7b50-b2bb-9f2fa007c110";

fn codex_in(owned: &Owned) -> Option<Agent> {
    let agents = owned.host.host().agents;
    agents
        .into_iter()
        .find(|agent| agent.runs_in() == Harness::Codex)
}

fn said(state: &str) -> String {
    format!("printf '\\033]7501;state={state}\\033\\\\'\n")
}

fn held_until(step: &Path) -> String {
    format!("while [ ! -f '{}' ]; do :; done\n", step.display())
}

#[test]
fn a_codex_in_lune_mux_tells_its_dialogs_its_status_and_the_thread_it_writes() {
    let mut owned = Owned::new();
    owned.spawn_mux(|_| {});
    let codex = owned.host.home.path().join(".codex");
    let locks = codex.join("thread-writer-locks");
    fs::create_dir_all(&locks).unwrap();
    let lock = locks.join(format!("{THREAD}.lock"));

    let dir = tempfile::tempdir().unwrap();
    let step = |number: u8| dir.path().join(format!("step{number}"));
    let script_path = dir.path().join("codex.sh");
    let body = [
        said("blocked:app=codex:kind=permission:id=startup/trust"),
        held_until(&step(1)),
        said("clear:id=startup/trust"),
        format!("exec 9>'{}'\n", lock.display()),
        said("idle:app=codex"),
        held_until(&step(2)),
        said("working:app=codex"),
        held_until(&step(3)),
        said("blocked:app=codex:kind=question"),
        held_until(&step(4)),
        said("clear"),
        held_until(&step(5)),
    ]
    .concat();
    script(&script_path, &body);
    owned.new_terminal_running(b"codex", vec![script_path.to_str().unwrap().to_owned()]);

    let asked = eventually("Codex's trust dialog blocks the agent itself", || {
        codex_in(&owned)
            .filter(|agent| matches!(AgentStatus::of_api(agent), AgentStatus::Blocked { .. }))
    });
    assert_eq!(
        AgentStatus::of_api(&asked),
        AgentStatus::Blocked {
            reason: BlockedReason::Permission,
            message: None,
        }
    );
    assert_eq!(asked.status_rung, Some(StatusRung::Osc7501.into()));
    assert_eq!(asked.subagents, [], "a dialog is no subagent");
    assert_eq!(
        asked.session_id, None,
        "Codex writes no thread before it is trusted"
    );
    let pid = asked.pid;

    fs::write(step(1), "").unwrap();
    let rested = eventually("the thread Codex holds a lock on is its session", || {
        codex_in(&owned).filter(|agent| agent.session_id.as_deref() == Some(THREAD))
    });
    assert_eq!(rested.pid, pid);
    assert_eq!(AgentStatus::of_api(&rested), AgentStatus::Stopped);
    assert_eq!(rested.name, None);

    let index = format!(
        "{{\"id\":\"{THREAD}\",\"thread_name\":\"Reply with hello\"}}\n\
         {{\"id\":\"{THREAD}\",\"thread_name\":\"lighthouse\"}}\n"
    );
    fs::write(codex.join("session_index.jsonl"), index).unwrap();
    let named = eventually("the thread's last name in Codex's index names it", || {
        codex_in(&owned).filter(|agent| agent.name.is_some())
    });
    let name = named.name.as_ref().unwrap();
    assert_eq!(name.auto(), Some("lighthouse"));
    assert_eq!(name.given(), None, "Codex names a thread itself, too");

    fs::write(step(2), "").unwrap();
    eventually("a prompt sets Codex working", || {
        codex_in(&owned).filter(|agent| AgentStatus::of_api(agent) == AgentStatus::Working)
    });

    fs::write(step(3), "").unwrap();
    let waits = eventually("a question blocks Codex", || {
        codex_in(&owned)
            .filter(|agent| matches!(AgentStatus::of_api(agent), AgentStatus::Blocked { .. }))
    });
    assert_eq!(
        AgentStatus::of_api(&waits),
        AgentStatus::Blocked {
            reason: BlockedReason::Question,
            message: None,
        }
    );
    assert_eq!(waits.session_id.as_deref(), Some(THREAD));

    fs::write(step(4), "").unwrap();
    eventually("Codex's clear on exit drops its row", || {
        codex_in(&owned).is_none().then_some(())
    });
    fs::write(step(5), "").unwrap();
}

#[test]
fn a_codex_resumed_on_a_thread_runs_it_before_it_writes_and_the_lock_says_what_it_writes() {
    let mut owned = Owned::new();
    owned.spawn_mux(|_| {});
    let locks = owned.host.home.path().join(".codex/thread-writer-locks");
    fs::create_dir_all(&locks).unwrap();
    let lock = locks.join(format!("{NEWER_THREAD}.lock"));

    let dir = tempfile::tempdir().unwrap();
    let step = |number: u8| dir.path().join(format!("step{number}"));
    let script_path = dir.path().join("codex.sh");
    let body = [
        said("idle:app=codex"),
        held_until(&step(1)),
        format!("exec 9>'{}'\n", lock.display()),
        said("working:app=codex"),
        held_until(&step(2)),
    ]
    .concat();
    script(&script_path, &body);
    let argv = [script_path.to_str().unwrap(), "resume", THREAD];
    owned.new_terminal_running(b"codex", argv.map(str::to_owned).to_vec());

    let resumed = eventually("a resumed Codex runs the thread it was started on", || {
        codex_in(&owned).filter(|agent| agent.session_id.is_some())
    });
    assert_eq!(resumed.session_id.as_deref(), Some(THREAD));
    assert_eq!(AgentStatus::of_api(&resumed), AgentStatus::Stopped);

    fs::write(step(1), "").unwrap();
    eventually(
        "the thread Codex writes outranks the one it was started on",
        || codex_in(&owned).filter(|agent| agent.session_id.as_deref() == Some(NEWER_THREAD)),
    );
    fs::write(step(2), "").unwrap();
}
