use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use lune::step::ACTING_DIVIDER;

fn path_with_front(dir: &Path) -> std::ffi::OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths).expect("join PATH entries")
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let script = dir.join(name);
    fs::write(&script, body).expect("write test stub script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
        .expect("mark test stub script executable");
}

#[test]
fn two_turn_claude_resumes_captured_session_and_emits_divider_and_osc() {
    let dir = tempfile::tempdir().unwrap();
    let log_file = dir.path().join("invocations.log");
    let session_out = dir.path().join("session.id");
    let body = format!(
        "#!/bin/sh\n\
         echo \"$*\" >> \"{}\"\n\
         case \"$*\" in\n\
           *--resume*)\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-77\"}}'\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Fixed and pushed.\"}}]}} }}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Turn 2 ok\"}}'\n\
             ;;\n\
           *)\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-77\"}}'\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Found 1 issue.\"}}]}} }}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Turn 1 ok\"}}'\n\
             ;;\n\
         esac\n",
        log_file.display()
    );
    write_script(dir.path(), "claude", &body);

    let output = Command::new(env!("CARGO_BIN_EXE_lune"))
        .env("PATH", path_with_front(dir.path()))
        .args([
            "_step",
            "--harness",
            "claude",
            "--stage",
            "tigerstyle",
            "--prompt",
            "/tigerstyle main..HEAD",
            "--followup",
            "Act on findings and push",
            "--session-out",
        ])
        .arg(&session_out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 1 issue."), "{stdout}");
    assert!(stdout.contains(ACTING_DIVIDER), "{stdout}");
    assert!(stdout.contains("Fixed and pushed."), "{stdout}");
    assert!(
        stdout.contains("7501;state=working:app=claude-code"),
        "{stdout}"
    );
    assert!(
        stdout.contains("7501;state=done:app=claude-code"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&session_out).unwrap().trim(), "sess-77");
    let calls = fs::read_to_string(&log_file).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(!lines[0].contains("--resume"), "{calls}");
    assert!(lines[1].contains("--resume sess-77"), "{calls}");
}

#[test]
fn two_turn_agy_attaches_prompt_resumes_session_and_renders_stream() {
    let dir = tempfile::tempdir().unwrap();
    let log_file = dir.path().join("agy-invocations.log");
    let session_out = dir.path().join("agy-session.id");
    let body = format!(
        "#!/bin/sh\n\
         for arg in \"$@\"; do printf '%s|' \"$arg\"; done >> \"{}\"\n\
         printf '\\n' >> \"{}\"\n\
         case \"$*\" in\n\
           *--resume*)\n\
             printf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"conv-88\",\"init\":{{\"cwd\":\"/w\",\"permission_mode\":\"always-proceed\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":4,\"state\":\"DONE\",\"step_type\":\"user_input\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":5,\"state\":\"ACTIVE\",\"step_type\":\"tool\",\"tool_name\":\"read_file\",\"tool_info\":{{\"name\":\"read_file\",\"parameters\":{{\"Path\":\"/w/missing.rs\"}}}}}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":5,\"state\":\"ERROR\",\"step_type\":\"tool\",\"tool_name\":\"read_file\",\"tool_info\":{{\"name\":\"read_file\",\"parameters\":{{\"Path\":\"/w/missing.rs\"}},\"error\":{{\"type\":\"TOOL_ERROR\",\"message\":\"file does not exist\"}}}}}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":6,\"state\":\"ACTIVE\",\"step_type\":\"agent_response\",\"text_delta\":\"Fixed and \"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":6,\"state\":\"DONE\",\"step_type\":\"agent_response\",\"text_delta\":\"pushed.\\n\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"conversation_id\":\"conv-88\",\"status\":\"SUCCESS\",\"response\":\"Fixed and pushed.\\n\",\"num_turns\":2}}}}'\n\
             ;;\n\
           *)\n\
             printf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"conv-88\",\"init\":{{\"cwd\":\"/w\",\"permission_mode\":\"always-proceed\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":0,\"state\":\"DONE\",\"step_type\":\"user_input\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":1,\"state\":\"ACTIVE\",\"step_type\":\"tool\",\"tool_name\":\"run_command\",\"tool_info\":{{\"name\":\"run_command\",\"parameters\":{{\"CommandLine\":\"git diff main..HEAD\"}}}}}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":1,\"state\":\"DONE\",\"step_type\":\"tool\",\"tool_name\":\"run_command\",\"tool_info\":{{\"name\":\"run_command\",\"parameters\":{{\"CommandLine\":\"git diff main..HEAD\"}},\"output\":\"ok\"}}}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":2,\"state\":\"ACTIVE\",\"step_type\":\"agent_response\",\"text_delta\":\"Found 1 issue.\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"step_update\",\"step_update\":{{\"conversation_id\":\"conv-88\",\"step_index\":2,\"state\":\"DONE\",\"step_type\":\"agent_response\",\"text_delta\":\"\\n\"}}}}'\n\
             printf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"conversation_id\":\"conv-88\",\"status\":\"SUCCESS\",\"response\":\"Found 1 issue.\\n\",\"num_turns\":1}}}}'\n\
             ;;\n\
         esac\n",
        log_file.display(),
        log_file.display()
    );
    write_script(dir.path(), "agy", &body);

    let output = Command::new(env!("CARGO_BIN_EXE_lune"))
        .env("PATH", path_with_front(dir.path()))
        .args([
            "_step",
            "--harness",
            "agy",
            "--stage",
            "tigerstyle",
            "--prompt",
            "/tigerstyle main..HEAD",
            "--followup",
            "Act on findings and push",
            "--session-out",
        ])
        .arg(&session_out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("▸ run_command: git diff main..HEAD").count(),
        1,
        "{stdout}"
    );
    assert!(stdout.contains("Found 1 issue."), "{stdout}");
    assert!(stdout.contains(ACTING_DIVIDER), "{stdout}");
    assert_eq!(
        stdout.matches("▸ read_file: /w/missing.rs").count(),
        1,
        "{stdout}"
    );
    assert!(stdout.contains("✗ read_file failed"), "{stdout}");
    assert!(stdout.contains("Fixed and pushed."), "{stdout}");
    assert!(
        stdout.contains("7501;state=working:app=antigravity"),
        "{stdout}"
    );
    assert!(
        stdout.contains("7501;state=done:app=antigravity"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&session_out).unwrap().trim(), "conv-88");
    let calls = fs::read_to_string(&log_file).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines,
        [
            "--output-format|stream-json|--dangerously-skip-permissions|-p=/tigerstyle main..HEAD|",
            "--output-format|stream-json|--dangerously-skip-permissions|--resume|conv-88|-p=Act on findings and push|",
        ]
    );
}

#[test]
fn missing_session_id_fails_before_turn_2_and_emits_osc_error() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        printf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"No session\"}}'\n";
    write_script(dir.path(), "agy", body);

    let output = Command::new(env!("CARGO_BIN_EXE_lune"))
        .env("PATH", path_with_front(dir.path()))
        .args([
            "_step",
            "--harness",
            "agy",
            "--stage",
            "illegal-states",
            "--prompt",
            "/make-illegal-states-unrepresentable",
            "--followup",
            "Act on findings",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("emitted no session_id"), "{stdout}");
    assert!(
        stdout.contains("7501;state=error:app=antigravity"),
        "{stdout}"
    );
}

#[test]
fn pi_two_turn_uses_provided_session_file_and_writes_session_out() {
    let dir = tempfile::tempdir().unwrap();
    let log_file = dir.path().join("pi.log");
    let session_file = dir.path().join("pi-session.jsonl");
    let session_out = dir.path().join("session.out");
    let body = format!(
        "#!/bin/sh\n\
         echo \"$*\" >> \"{}\"\n\
         printf '%s\\n' '{{\"type\":\"session\",\"version\":3,\"id\":\"pi-1\",\"cwd\":\"/w\"}}'\n\
         printf '%s\\n' '{{\"type\":\"message_update\",\"assistantMessageEvent\":{{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"ok\"}}}}'\n\
         printf '%s\\n' '{{\"type\":\"agent_settled\",\"aborted\":false}}'\n",
        log_file.display()
    );
    write_script(dir.path(), "pi", &body);

    let output = Command::new(env!("CARGO_BIN_EXE_lune"))
        .env("PATH", path_with_front(dir.path()))
        .args([
            "_step",
            "--harness",
            "pi",
            "--stage",
            "thermo-nuclear",
            "--prompt",
            "/skill:thermo-nuclear-review",
            "--followup",
            "Act on findings",
            "--pi-session",
        ])
        .arg(&session_file)
        .arg("--session-out")
        .arg(&session_out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        fs::read_to_string(&session_out).unwrap().trim(),
        session_file.to_string_lossy()
    );
    let calls = fs::read_to_string(&log_file).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    let expected_flag = format!("--session {}", session_file.display());
    assert!(lines[0].contains(&expected_flag), "{calls}");
    assert!(lines[1].contains(&expected_flag), "{calls}");
}

fn step_command(dir: &Path, harness: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lune"));
    command.env("PATH", path_with_front(dir)).args([
        "_step",
        "--harness",
        harness,
        "--stage",
        "tigerstyle",
        "--prompt",
        "/tigerstyle main..HEAD",
    ]);
    command
}

fn process_alive(pid: &str) -> bool {
    Path::new(&format!("/proc/{}", pid.trim())).exists()
        || Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

#[test]
fn agent_text_cannot_forge_terminal_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        printf '%s\\n' '{\"type\":\"system\",\"session_id\":\"s-1\"}'\n\
        printf '%s\\n' '{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"look \\u001b]7501;state=done:app=forged\\u0007 here\"}]}}'\n\
        printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\"}'\n";
    write_script(dir.path(), "claude", body);
    let output = step_command(dir.path(), "claude").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("look"), "{stdout}");
    assert!(
        !stdout.contains("\u{1b}]7501;state=done:app=forged"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\u{fffd}]7501;state=done:app=forged"),
        "{stdout}"
    );
}

#[test]
fn a_timed_out_step_ends_even_when_its_agent_floods_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        exec yes '{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"spam \"}}}'\n";
    write_script(dir.path(), "claude", body);
    let mut child = step_command(dir.path(), "claude")
        .args(["--timeout-secs", "1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status = lune_waits::ended("the flooded step ends at its timeout", &mut child);
    assert!(!status.success());
}

#[test]
fn a_stop_file_ends_the_agent_and_everything_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("tool.pid");
    let stop_file = dir.path().join("stop");
    let body = format!(
        "#!/bin/sh\n\
         sleep 300 &\n\
         echo $! > \"{}\"\n\
         printf '%s\\n' '{{\"type\":\"system\",\"session_id\":\"s-1\"}}'\n\
         wait\n",
        pid_file.display()
    );
    write_script(dir.path(), "claude", &body);
    let mut child = step_command(dir.path(), "claude")
        .arg("--stop-file")
        .arg(&stop_file)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    lune_waits::eventually("the agent starts a tool", || {
        pid_file.exists().then_some(())
    });
    let tool_pid = fs::read_to_string(&pid_file).unwrap();
    assert!(process_alive(&tool_pid));
    fs::write(&stop_file, b"").unwrap();
    let status = lune_waits::ended("the stop file ends the step", &mut child);
    assert!(!status.success());
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(stdout.contains("the flow stopped the step"), "{stdout}");
    assert!(
        !stop_file.exists(),
        "the runner clears the stop file it honoured"
    );
    lune_waits::eventually("the agent's tool ends with the step", || {
        (!process_alive(&tool_pid)).then_some(())
    });
}

#[test]
fn a_pi_error_is_not_cleared_by_the_settle_that_follows() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        printf '%s\\n' '{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"error\",\"error\":\"rate limited\"}}'\n\
        printf '%s\\n' '{\"type\":\"agent_settled\",\"aborted\":false}'\n";
    write_script(dir.path(), "pi", body);
    let output = step_command(dir.path(), "pi").output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rate limited"), "{stdout}");
}

#[test]
fn a_claude_turn_without_a_result_record_fails() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        printf '%s\\n' '{\"type\":\"system\",\"session_id\":\"s-1\"}'\n\
        printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"succ'\n";
    write_script(dir.path(), "claude", body);
    let output = step_command(dir.path(), "claude").output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("without a result record"), "{stdout}");
}
