use std::io::Write as _;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use lunetad::api::say::Does;
use lunetad::api::{self, Saying};

use crate::host::{answered, folder_in, start, Host, LUNETA};

fn luneta(host: &Host, args: &[&str]) -> Output {
    answered(host.vars().command(LUNETA).args(args))
}

type Read<'a> = (&'static str, &'a str);

fn read(saying: &Saying) -> Read<'_> {
    let does = saying.say.as_ref().and_then(|say| say.does.as_ref()).unwrap();
    match does {
        Does::InTurn(utterance) => ("in turn", &utterance.text),
        Does::Now(utterance) => ("now", &utterance.text),
        Does::Stop(()) => ("stop", ""),
    }
}

fn said(host: &api::Host) -> Vec<Read<'_>> {
    host.sayings.iter().map(read).collect()
}

fn succeeds(output: &Output) {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn luneta_say_queues_what_it_is_given_and_returns() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let before = host.host();
    assert_eq!(said(&before), []);

    succeeds(&luneta(&host, &["say", "the tests pass"]));
    succeeds(&luneta(&host, &["say", "--now", "look here"]));
    succeeds(&luneta(&host, &["say", "--stop"]));

    let after = host.host();
    assert_ne!(after.version, before.version, "a window waiting hears of it at once");
    let want = [("in turn", "the tests pass"), ("now", "look here"), ("stop", "")];
    assert_eq!(said(&after), want);
    let numbers: Vec<u64> = after.sayings.iter().map(|saying| saying.number).collect();
    assert!(numbers.is_sorted(), "{numbers:?}");
    assert!(after.sayings.iter().all(|saying| saying.pid == after.version.unwrap().pid));
}

#[test]
fn luneta_say_takes_what_comes_on_stdin() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let mut command = host.vars().command(LUNETA);
    command.arg("say").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"from a pipe\n").unwrap();
    succeeds(&child.wait_with_output().unwrap());
    let host = host.host();
    assert_eq!(said(&host)[0].1, "from a pipe\n");
}

#[test]
fn what_says_nothing_or_too_much_is_refused() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let blank = luneta(&host, &["say", "  "]);
    assert!(!blank.status.success());
    assert!(String::from_utf8_lossy(&blank.stderr).contains("it says nothing"));
    let long = "a".repeat(lunetad::sayings::BYTES_MAX + 1);
    let long = luneta(&host, &["say", &long]);
    assert!(!long.status.success());
    assert!(String::from_utf8_lossy(&long.stderr).contains("the most to say at once"));
    assert_eq!(said(&host.host()), []);
}

#[test]
fn with_no_lunetad_it_fails_at_once_and_starts_none() {
    let host = Host::new();
    let began = Instant::now();
    let failed = luneta(&host, &["say", "anyone there"]);
    assert!(!failed.status.success());
    assert!(began.elapsed() < Duration::from_secs(4), "{:?}", began.elapsed());
    let why = String::from_utf8_lossy(&failed.stderr);
    assert!(why.contains("lunetad"), "{why}");
    assert!(folder_in(host.runtime.path()).is_none(), "it started a daemon");
}
