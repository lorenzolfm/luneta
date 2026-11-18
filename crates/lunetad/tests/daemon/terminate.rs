use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use lunetad::api::projects::Held;
use lunetad::mux::own::api::terminal::Program;
use rustix::process::{test_kill_process, Pid};

use crate::host::{answered, eventually, start, Host, Running, Vars, LUNETA};
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
    assert_eq!(far_line, "far: ends lunetad; forgets 1 project");
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
    assert_eq!(far_line, "far: ended lunetad; forgot 1 project");
    assert_eq!(gone_again, gone_line);
    assert!(far_daemon.ended_within(ENDS).is_some(), "far's lunetad runs on");
    assert!(!kept(&far).exists());
    assert!(!Path::new(&kept(&here_host)).exists());
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
