use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use lunetad::api::projects::Held;
use lunetad::mux::own::api::terminal::Program;
use rustix::process::{test_kill_process, Pid};

use crate::host::{answered, eventually, start, Host, Running, Vars, LUNETA, LUNETAD};
use crate::own::{built_server, Owned};
use crate::projects::{kept, project, workstream};
use crate::remote::{hosts, FakeSsh};

const ENDS: Duration = Duration::from_secs(5);

struct Said {
    code: Option<i32>,
    lines: Vec<String>,
    stderr: String,
}

fn terminate(vars: Vars, yes: Option<&str>) -> Said {
    let mut command = vars.command(LUNETA);
    command.arg("terminate").stdin(Stdio::null());
    if let Some(plan) = yes {
        command.args(["--yes", plan]);
    }
    let output = answered(&mut command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    Said {
        code: output.status.code(),
        lines: stdout.lines().map(str::to_owned).collect(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn plan_in(said: &Said) -> String {
    let (_, plan) = said.stderr.split_once("--yes ").expect("a plan is said to agree to");
    plan.split_whitespace().next().unwrap().to_owned()
}

fn here(line: &str) -> &str {
    let (_, said) = line.split_once(" (this machine): ").expect("this machine's line comes first");
    said
}

fn gone(pid: u32) -> bool {
    test_kill_process(Pid::from_raw(i32::try_from(pid).unwrap()).unwrap()).is_err()
}

fn kept_in_home(host: &Host) {
    let home = fs::canonicalize(host.home.path()).unwrap();
    let work = home.join("work");
    fs::create_dir_all(&work).unwrap();
    workstream(host, project(host, &home, None), &work);
}

#[test]
fn luneta_terminate_ends_lunetad_the_mux_its_terminals_and_lunetas_and_forgets_the_projects() {
    let mut owned = Owned::new();
    kept_in_home(&owned.host);
    let server = built_server();
    let mut mux = owned.host.vars().command(server.to_str().unwrap());
    mux.arg(owned.host.folder());
    let mut mux = Running::spawn(mux.stdin(Stdio::null()));
    let socket = owned.socket();
    eventually("the server listens", || socket.exists().then_some(()));
    owned.new_terminal(b"kept");
    let listing = owned.listing();
    let Some(Program::Runs(runs)) = &listing.terminals[0].program else {
        panic!("the terminal runs: {listing:?}");
    };
    let shell = runs.pid;
    let mut bridge = owned.host.vars().command(LUNETA);
    bridge.arg("bridge").stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut bridge = Running::spawn(&mut bridge);
    let _held = bridge.piped();
    let elsewhere = Host::new();
    let mut untouched = start(elsewhere.vars()).unwrap();

    let planned = terminate(owned.host.vars(), None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let says = "ends 1 luneta command, lunetad and luneta-mux; forgets 1 project and 1 workstream";
    assert_eq!(planned.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);
    assert!(mux.ended_within(Duration::ZERO).is_none(), "a plan ends nothing");
    assert!(kept(&owned.host).exists());

    let ended = terminate(owned.host.vars(), Some(&plan_in(&planned)));
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let says = "ended 1 luneta command, lunetad and luneta-mux; forgot 1 project and 1 workstream";
    assert_eq!(ended.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);
    assert!(owned.daemon_ended_within(ENDS).is_some(), "lunetad runs on");
    assert!(mux.ended_within(ENDS).is_some(), "luneta-mux runs on");
    assert!(bridge.ended_within(ENDS).is_some(), "luneta bridge runs on");
    eventually("the terminal's program ends with luneta-mux", || gone(shell).then_some(()));
    assert!(!kept(&owned.host).exists());
    let ended_elsewhere = untouched.ended_within(Duration::ZERO);
    assert!(ended_elsewhere.is_none(), "the lunetad of another folder ended");

    let again = terminate(owned.host.vars(), None);
    assert_eq!(again.code, Some(3), "{}", again.stderr);
    let says = "nothing of luneta runs; keeps no project to forget";
    assert_eq!(again.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);
    let _daemon = start(owned.host.vars()).unwrap();
    let held = owned.host.host().projects.and_then(|projects| projects.held);
    let Some(Held::Kept(kept)) = held else {
        panic!("a lunetad started again keeps projects: {held:?}");
    };
    assert!(kept.projects.is_empty(), "{:?}", kept.projects);
}

#[test]
fn luneta_terminate_ends_the_lunetad_that_keeps_the_projects_from_another_folder() {
    let host = Host::new();
    let mut first = start(host.vars()).unwrap();
    let other = tempfile::tempdir().unwrap();
    let there = Vars { runtime: Some(other.path()), ..host.vars() };

    let planned = terminate(there, None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let [line] = &planned.lines[..] else { panic!("one machine: {:?}", planned.lines) };
    assert!(here(line).starts_with("ends lunetad"), "{line}");

    let ended = terminate(there, Some(&plan_in(&planned)));
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    assert!(first.ended_within(ENDS).is_some(), "the lunetad that keeps the projects runs on");
}

#[test]
fn luneta_terminate_ends_luneta_on_each_machine_ssh_reaches_and_skips_the_others() {
    let (here_host, far) = (Host::new(), Host::new());
    let ssh = FakeSsh::new(here_host.zellij.bin());
    ssh.reaches("far", far.vars());
    ssh.runs_luneta("far", far.vars());
    hosts(&here_host, "far\ngone\n");
    let mut far_daemon = start(far.vars()).unwrap();
    let home = fs::canonicalize(far.home.path()).unwrap();
    project(&far, &home, None);

    let planned = terminate(here_host.vars(), None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let [local, far_line, gone_line] = &planned.lines[..] else {
        panic!("a line for each machine: {:?}", planned.lines);
    };
    assert_eq!(here(local), "nothing of luneta runs; keeps no project to forget");
    assert_eq!(far_line, "far: ends lunetad; forgets 1 project and 1 workstream");
    let skipped = "gone: skipped, as ssh does not reach it, and so luneta runs on there and keeps \
                   its projects: ssh: Could not resolve hostname gone";
    assert!(gone_line.starts_with(skipped), "{gone_line}");
    assert!(far_daemon.ended_within(Duration::ZERO).is_none(), "a plan ends nothing");

    let ended = terminate(here_host.vars(), Some(&plan_in(&planned)));
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let [local, far_line, gone_again] = &ended.lines[..] else {
        panic!("a line for each machine: {:?}", ended.lines);
    };
    assert_eq!(here(local), "nothing of luneta ran; kept no project to forget");
    assert_eq!(far_line, "far: ended lunetad; forgot 1 project and 1 workstream");
    assert_eq!(gone_again, gone_line);
    assert!(far_daemon.ended_within(ENDS).is_some(), "far's lunetad runs on");
    assert!(!kept(&far).exists());
    assert!(!Path::new(&kept(&here_host)).exists());
}

fn named_bash(name: &str, at: &Path) -> std::path::PathBuf {
    let found = Command::new("sh").args(["-c", "command -v bash"]).output().unwrap();
    let bash = String::from_utf8(found.stdout).unwrap();
    let named = at.join(name);
    fs::copy(bash.trim(), &named).unwrap();
    named
}

#[test]
fn luneta_terminate_forgets_again_what_a_lunetad_started_meanwhile_kept() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    kept_in_home(&host);
    let scratch = tempfile::tempdir().unwrap();
    let saved = scratch.path().join("saved");
    fs::copy(kept(&host), &saved).unwrap();
    let mux = named_bash("luneta-mux", scratch.path());
    let log = scratch.path().join("lunetad.log");
    let up = scratch.path().join("up");
    let script = "trap 'kill \"$idle\"; cp \"$1\" \"$2\"; \"$3\" >/dev/null 2>\"$4\" & \
                  until grep -q \"listening on\" \"$4\" 2>/dev/null; do sleep 0.05; done; \
                  exit' TERM; sleep 3600 & idle=$!; : >\"$5\"; wait \"$idle\"";
    let mut writer = host.vars().command(mux.to_str().unwrap());
    writer.args(["-c", script, "luneta-mux"]);
    let (kept_at, lunetad) = (kept(&host), Path::new(LUNETAD));
    let paths = [saved.as_path(), kept_at.as_path(), lunetad, log.as_path(), up.as_path()];
    writer.args(paths);
    let mut writer = Running::spawn(writer.stdin(Stdio::null()).stdout(Stdio::null()));
    eventually("the writer is up", || up.exists().then_some(()));

    let planned = terminate(host.vars(), None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let says = "ends lunetad and luneta-mux; forgets 1 project and 1 workstream";
    assert_eq!(planned.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);

    let ended = terminate(host.vars(), Some(&plan_in(&planned)));
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let says = "ended 2 lunetads and luneta-mux; forgot 1 project and 1 workstream";
    assert_eq!(ended.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);
    assert!(daemon.ended_within(ENDS).is_some(), "lunetad runs on");
    assert!(writer.ended_within(ENDS).is_some(), "luneta-mux runs on");
    assert!(fs::read_to_string(&log).unwrap().contains("listening on"), "no lunetad started");
    assert!(!kept(&host).exists(), "what the lunetad started meanwhile kept is kept still");
}

#[test]
fn luneta_terminate_says_each_line_of_the_hosts_file_that_names_no_machine() {
    let host = Host::new();
    hosts(&host, "two words\n");
    let planned = terminate(host.vars(), None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let [local, unread] = &planned.lines[..] else {
        panic!("a line for this machine and one for the line: {:?}", planned.lines);
    };
    assert_eq!(here(local), "nothing of luneta runs; keeps no project to forget");
    let skipped = "hosts file: skipped a line, as it names no machine ssh could be asked of, and \
                   so any luneta it meant runs on there and keeps its projects: \"two words\" is \
                   more than one word";
    assert_eq!(unread, skipped);

    hosts(&host, "three more words\n");
    let refused = terminate(host.vars(), Some(&plan_in(&planned)));
    assert_eq!(refused.code, Some(3), "{}", refused.stderr);
    assert!(refused.stderr.contains("told yes to another plan than this"), "{}", refused.stderr);

    let ended = terminate(host.vars(), Some(&plan_in(&refused)));
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let [_, unread] = &ended.lines[..] else {
        panic!("a line for this machine and one for the line: {:?}", ended.lines);
    };
    assert!(unread.ends_with("\"three more words\" is more than one word"), "{unread}");
}

fn here_ending(vars: Vars, key: &str, set: &[(&str, &str)]) -> Said {
    let mut command = vars.command(LUNETA);
    command.args(["terminate", "--here", "--ending", key]).stdin(Stdio::null());
    command.envs(set.iter().copied());
    let output = answered(&mut command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    Said {
        code: output.status.code(),
        lines: stdout.lines().map(str::to_owned).collect(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn luneta_terminate_here_ends_nothing_in_luneta_s_terminal_or_on_a_key_that_changed() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    let mut asked = host.vars().command(LUNETA);
    let asked = answered(asked.args(["terminate", "--here"]).stdin(Stdio::null()));
    assert!(asked.status.success(), "{}", String::from_utf8_lossy(&asked.stderr));
    let stdout = String::from_utf8(asked.stdout).unwrap();
    let key = stdout.lines().next().expect("a key comes first").to_owned();

    let terminal = [("LUNETA_MUX_TERMINAL", "7"), ("LUNETA_MUX_TERMINAL_MADE", "60")];
    let zellij = [("ZELLIJ_SESSION_NAME", "work"), ("ZELLIJ_PANE_ID", "2")];
    for set in [&terminal[..], &zellij[..]] {
        let refused = here_ending(host.vars(), &key, set);
        assert_eq!(refused.code, Some(1), "{set:?}: {}", refused.stderr);
        assert!(refused.stderr.contains("runs in a terminal of luneta's"), "{}", refused.stderr);
        assert!(refused.lines.is_empty(), "{:?}", refused.lines);
    }
    let stale = here_ending(host.vars(), "0000000000000000", &[]);
    assert_eq!(stale.code, Some(1), "{}", stale.stderr);
    assert!(stale.stderr.contains("changed since: ends lunetad"), "{}", stale.stderr);
    assert!(daemon.ended_within(Duration::from_millis(500)).is_none(), "lunetad was ended");

    let ended = here_ending(host.vars(), &key, &[]);
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    assert_eq!(ended.lines, ["ended lunetad; kept no project to forget"]);
    assert!(daemon.ended_within(ENDS).is_some(), "lunetad runs on");
}

#[test]
fn luneta_terminate_ends_nothing_on_a_plan_that_changed_since_it_was_said() {
    let host = Host::new();
    let mut daemon = start(host.vars()).unwrap();
    let planned = terminate(host.vars(), None);
    assert_eq!(planned.code, Some(3), "{}", planned.stderr);
    let plan = plan_in(&planned);
    kept_in_home(&host);

    let refused = terminate(host.vars(), Some(&plan));
    assert_eq!(refused.code, Some(3), "{}", refused.stderr);
    let says = "ends lunetad; forgets 1 project and 1 workstream";
    assert_eq!(refused.lines.iter().map(|line| here(line)).collect::<Vec<_>>(), [says]);
    assert!(refused.stderr.contains("told yes to another plan than this"), "{}", refused.stderr);
    assert_ne!(plan_in(&refused), plan);
    assert!(daemon.ended_within(Duration::from_millis(500)).is_none(), "lunetad was ended");
    assert!(kept(&host).exists());
}
