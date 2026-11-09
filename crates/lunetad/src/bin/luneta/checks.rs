//! The checks that failed on the head of a pane's pull request, for the agent
//! that the menu's `c c` starts beside it, to fix them: gh says them, with the
//! log of each job of GitHub Actions that failed, as `lunetad::checks` keeps
//! them, on the machine the pane is on.

use std::sync::mpsc::Sender;
use std::thread;

pub use lunetad::checks::{Failed, Failing, Log, PullPage};
use lunetad::printed::Kept;

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;


/// What an agent is told of `failing`, the checks of the pull request of
/// `page`: what to do, and where each log is, or why there is none, and how
/// many have yet to end.
pub fn prompt(page: &PullPage, failing: &Failing) -> String {
    let Failing { head, failed, pending, .. } = failing;
    let (number, url) = (page.number(), page.url());
    let asks = format!("Fix the checks that failed on pull request #{number}, {url}, at its head");
    let mut said = vec![format!("{asks} {head}.")];
    said.extend(failed.iter().map(|Failed { name, link, log }| match log {
        Log::Kept(Kept { file, left_out: 0, .. }) => {
            format!("`{name}` failed: its log is in {file}.")
        },
        Log::Kept(Kept { file, lines, left_out }) => {
            let of = lines + left_out;
            format!("`{name}` failed: the last {lines} of the {of} lines of its log are in {file}.")
        },
        Log::Unkept(why) => match link {
            Some(link) => format!("`{name}` failed, with no log: {why}. Its page is {link}."),
            None => format!("`{name}` failed, with no log: {why}."),
        },
    }));
    match pending {
        0 => {},
        1 => said.push("One other check has yet to end.".to_owned()),
        _ => said.push(format!("{pending} other checks have yet to end.")),
    }
    said.join(" ")
}

/// The files `failing` has the agent read.
pub fn files(failing: &Failing) -> Vec<&str> {
    failing.failed.iter().filter_map(kept_file).collect()
}

/// The file the log of `failed` is kept in, where it is.
fn kept_file(failed: &Failed) -> Option<&str> {
    match &failed.log {
        Log::Kept(kept) => Some(&kept.file),
        Log::Unkept(_) => None,
    }
}

/// Has gh on `on` say the checks of the pull request of `page`, and keep the
/// log of each that failed, there, for the menu numbered `menu`, on a thread
/// of its own, and hands back what it found, or why it found nothing, as
/// `Due` does.
pub fn fetch(menu: u64, on: &Machine, page: PullPage, events: &Sender<Event>) {
    let on = on.clone();
    let due = Due::new(events, move |checks| Event::Checks { menu, checks });
    thread::spawn(move || {
        let checks = match &on {
            Machine::Local => lunetad::checks::kept_failing(&page),
            Machine::Remote(destination) => {
                Away::reach(destination, away::ANSWER).and_then(|mut away| away.checks(&page))
            },
        };
        due.pay(checks);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(file: &str, lines: usize, left_out: usize) -> Log {
        Log::Kept(Kept { file: file.to_owned(), lines, left_out })
    }

    fn failed(name: &str, link: Option<&str>, log: Log) -> Failed {
        Failed { name: name.to_owned(), link: link.map(str::to_owned), log }
    }

    /// The agent is told the pull request and its head, then each check that
    /// failed, with where its log is and how much of it, or why it has none
    /// and its page, and how many others have yet to end.
    #[test]
    fn the_agent_is_told_each_check_that_failed_and_where_its_log_is() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/328").unwrap();
        let failing = Failing {
            head: "abc123".to_owned(),
            failed: vec![
                failed("ci / clippy", Some("https://x/job/1"), kept("/run/checks/1.log", 12, 0)),
                failed("ci / mac", None, kept("/run/checks/2.log", 2000, 7)),
                failed("ext", Some("https://ci.example.com/1"), Log::Unkept("no job".to_owned())),
                failed("gone", None, Log::Unkept("no job".to_owned())),
            ],
            pending: 2,
            passed: 3,
        };
        assert_eq!(
            prompt(&page, &failing),
            "Fix the checks that failed on pull request #328, \
             https://github.com/owner/repo/pull/328, at its head abc123. `ci / clippy` failed: \
             its log is in /run/checks/1.log. `ci / mac` failed: the last 2000 of the 2007 \
             lines of its log are in /run/checks/2.log. `ext` failed, with no log: no job. Its \
             page is https://ci.example.com/1. `gone` failed, with no log: no job. 2 other \
             checks have yet to end."
        );
        assert_eq!(files(&failing), ["/run/checks/1.log", "/run/checks/2.log"]);

        let one = Failing { pending: 1, failed: failing.failed[..1].to_vec(), ..failing.clone() };
        assert!(prompt(&page, &one).ends_with(" One other check has yet to end."));
        let none = Failing { pending: 0, ..one };
        assert!(prompt(&page, &none).ends_with("/run/checks/1.log."));
    }
}
