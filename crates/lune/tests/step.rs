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
fn missing_session_id_fails_before_turn_2_and_emits_osc_error() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        printf '%s\\n' '{\"type\":\"result\",\"status\":\"SUCCESS\",\"response\":\"No session\"}'\n";
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
