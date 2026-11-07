//! What `luneta say` gives lunetad, for luneta's window to read aloud, as
//! `Watch` lists it: from a pane, by the name its session has now, from any
//! machine, and at once.

use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixListener;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use lunetad::api::say::Does;
use lunetad::api::{self, Saying};

use crate::agents::{self, file_for, now_in, process};
use crate::host::{answered, eventually, folder_in, start, Host, LUNETA};

/// The CLI, run with `args` outside zellij.
fn luneta(host: &Host, args: &[&str]) -> Output {
    answered(host.vars().command(LUNETA).args(args))
}

/// How a saying is to be read, what it reads, and whose pane it comes from: the
/// name the pane knows its session by, the name it has now, and the pane.
type Read<'a> = (&'static str, &'a str, Option<(&'a str, Option<&'a str>, u32)>);

fn read(saying: &Saying) -> Read<'_> {
    let does = saying.say.as_ref().and_then(|say| say.does.as_ref()).unwrap();
    let (how, utterance) = match does {
        Does::InTurn(utterance) => ("in turn", utterance),
        Does::Now(utterance) => ("now", utterance),
        Does::Stop(()) => return ("stop", "", None),
    };
    let speaker = utterance.speaker.as_ref();
    let from = speaker.map(|it| (it.named.as_str(), it.session.as_deref(), it.pane));
    (how, &utterance.text, from)
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
    let want =
        [("in turn", "the tests pass", None), ("now", "look here", None), ("stop", "", None)];
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

/// A session's name is said too, so it is held to what the text is, and it has
/// to be one, whatever the variables of the pane say.
#[test]
fn a_session_by_what_is_no_name_or_too_long_a_one_is_refused() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let from = |session: &str| {
        let mut command = host.vars().command(LUNETA);
        command.env("ZELLIJ_SESSION_NAME", session).env("ZELLIJ_PANE_ID", "2");
        answered(command.args(["say", "hello"]))
    };
    let long = from(&"a".repeat(lunetad::sayings::BYTES_MAX));
    assert!(!long.status.success());
    assert!(String::from_utf8_lossy(&long.stderr).contains("the most to say at once"));
    let no_name = from("a/b");
    assert!(!no_name.status.success());
    assert!(String::from_utf8_lossy(&no_name.stderr).contains("speaker"));
    assert_eq!(said(&host.host()), []);
}

/// An agent goes on in silence: nothing waits for a daemon that may never be.
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

/// zellij's variables keep the name a session had as the pane opened, and a
/// pane needs no agent in it for the daemon to say what that is now. Where it
/// cannot tell, as of a name no session has held, or that two have, the session
/// is left unsaid: a pane's id is not unique across them.
#[test]
fn what_comes_from_a_pane_goes_by_the_name_its_session_has_now_where_that_can_be_told() {
    let host = agents::Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let agent = process(Some(("work", 2)));
    host.write(agent.pid(), &file_for("agent", &agent, "busy"));
    let _daemon = start(host.vars()).unwrap();
    let in_session = |session| host.agent("agent").filter(|agent| now_in(agent) == Some(session));
    eventually("the daemon has the agent in work", || in_session("work"));
    fs::rename(host.contract().join("work"), host.contract().join("fix")).unwrap();
    eventually("the agent is in fix", || in_session("fix"));

    let say = |session: &str, pane: &str, text: &str| {
        let mut command = host.vars().command(LUNETA);
        command.env("ZELLIJ_SESSION_NAME", session).env("ZELLIJ_PANE_ID", pane);
        succeeds(&answered(command.args(["say", text])));
    };
    say("work", "2", "from an agent's pane");
    say("work", "9", "from a shell's pane");
    say("elsewhere", "9", "from a session unheard of");
    assert_eq!(
        said(&host.host()),
        [
            ("in turn", "from an agent's pane", Some(("work", Some("fix"), 2))),
            ("in turn", "from a shell's pane", Some(("work", Some("fix"), 9))),
            ("in turn", "from a session unheard of", Some(("elsewhere", None, 9))),
        ],
    );

    let _again = UnixListener::bind(host.contract().join("work")).unwrap();
    let named = |name: &str, host: &agents::Host| {
        host.host().sessions.iter().any(|session| session.name == name)
    };
    eventually("a new session took the name", || named("work", &host).then_some(()));
    say("work", "2", "from either");
    let told = host.host();
    assert_eq!(said(&told).last(), Some(&("in turn", "from either", Some(("work", None, 2)))));
}
