use std::sync::mpsc::Sender;
use std::thread;

pub use luned::checks::{Failed, Failing, Log, PullPage};
use luned::printed::Kept;

use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::reach::{self, Luned};

pub fn prompt(page: &PullPage, failing: &Failing) -> String {
    let Failing {
        head,
        failed,
        pending,
        ..
    } = failing;
    let (number, url) = (page.number(), page.url());
    let asks = format!("Fix the checks that failed on pull request #{number}, {url}, at its head");
    let mut said = vec![format!("{asks} {head}.")];
    said.extend(failed.iter().map(|Failed { name, link, log }| match log {
        Log::Kept(Kept {
            file, left_out: 0, ..
        }) => {
            format!("`{name}` failed: its log is in {file}.")
        }
        Log::Kept(Kept {
            file,
            lines,
            left_out,
        }) => {
            let of = lines + left_out;
            format!("`{name}` failed: the last {lines} of the {of} lines of its log are in {file}.")
        }
        Log::Unkept(why) => match link {
            Some(link) => format!("`{name}` failed, with no log: {why}. Its page is {link}."),
            None => format!("`{name}` failed, with no log: {why}."),
        },
    }));
    match pending {
        0 => {}
        1 => said.push("One other check has yet to end.".to_owned()),
        _ => said.push(format!("{pending} other checks have yet to end.")),
    }
    said.join(" ")
}

pub fn files(failing: &Failing) -> Vec<&str> {
    failing.failed.iter().filter_map(kept_file).collect()
}

fn kept_file(failed: &Failed) -> Option<&str> {
    match &failed.log {
        Log::Kept(kept) => Some(&kept.file),
        Log::Unkept(_) => None,
    }
}

pub fn fetch(menu: u64, on: &Machine, page: PullPage, events: &Sender<Event>) {
    let on = on.clone();
    let due = Due::new(events, move |checks| Event::Checks { menu, checks });
    thread::spawn(move || {
        due.pay(Luned::on(&on, reach::ANSWER).and_then(|mut luned| luned.checks(&page)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(file: &str, lines: usize, left_out: usize) -> Log {
        Log::Kept(Kept {
            file: file.to_owned(),
            lines,
            left_out,
        })
    }

    fn failed(name: &str, link: Option<&str>, log: Log) -> Failed {
        Failed {
            name: name.to_owned(),
            link: link.map(str::to_owned),
            log,
        }
    }

    #[test]
    fn the_agent_is_told_each_check_that_failed_and_where_its_log_is() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/328").unwrap();
        let failing = Failing {
            head: "abc123".to_owned(),
            failed: vec![
                failed(
                    "ci / clippy",
                    Some("https://x/job/1"),
                    kept("/run/checks/1.log", 12, 0),
                ),
                failed("ci / mac", None, kept("/run/checks/2.log", 2000, 7)),
                failed(
                    "ext",
                    Some("https://ci.example.com/1"),
                    Log::Unkept("no job".to_owned()),
                ),
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

        let one = Failing {
            pending: 1,
            failed: failing.failed[..1].to_vec(),
            ..failing.clone()
        };
        assert!(prompt(&page, &one).ends_with(" One other check has yet to end."));
        let none = Failing { pending: 0, ..one };
        assert!(prompt(&page, &none).ends_with("/run/checks/1.log."));
    }
}
