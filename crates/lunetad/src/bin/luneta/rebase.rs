use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use lunetad::rebases::{self, Came, Rebased, Stopped, Unrebased};
use lunetad::Directory;

use crate::agents::{self, Kept};
use crate::away::{self, Away};
use crate::agreed;
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::start::UNFINISHED;
use crate::worktrees::{self, Landed, Opening};

pub const IN_DOUBT: &str = "no telling how the rebase ended";

pub fn run(dir: Option<&Path>, path: &Path) -> ExitCode {
    let dir = match agreed::directory(dir) {
        Ok(dir) => dir,
        Err(why) => return failed(&why),
    };
    let rebased = match rebases::rebase(&dir, None) {
        Ok(rebased) => rebased,
        Err(Unrebased::Refused(why)) => return failed(&format!("no rebase: {why}")),
        Err(Unrebased::InDoubt(why)) => {
            eprintln!("luneta: {IN_DOUBT}, and it may wait for you: {why}");
            return ExitCode::from(UNFINISHED);
        },
    };
    let Came::Stopped(stopped) = &rebased.came else {
        println!("{}", said(&rebased));
        return ExitCode::SUCCESS;
    };
    eprintln!("luneta: {}", said(&rebased));
    let words = rebases::prompt(&rebased.branch, &rebased.onto, stopped);
    let agent = agents::on_words(Kept::shared().0.harness(), words, &[]);
    match handed(path, &dir, agent) {
        Ok(()) => eprintln!("luneta: an agent resolves it, in a view of the worktree"),
        Err(why) => eprintln!("luneta: no agent started, and the rebase waits for you: {why}"),
    }
    ExitCode::from(UNFINISHED)
}

fn handed(path: &Path, dir: &Directory, agent: Vec<String>) -> Result<(), String> {
    let folder = worktrees::real(Path::new(dir.as_str()))?;
    match worktrees::worked_in(path, &folder, agent.clone())? {
        Landed::Made(_) => Ok(()),
        Landed::Found(worktree) => worktrees::opened(path, worktree, agent, Opening::Beside),
    }
}

fn failed(why: &str) -> ExitCode {
    eprintln!("luneta: {why}");
    ExitCode::FAILURE
}

pub fn start(
    menu: u64,
    on: Machine,
    dir: Directory,
    branch: String,
    events: &Sender<Event>,
) -> JoinHandle<()> {
    let due = Due::new(events, move |rebased: Result<Result<Rebased, Unrebased>, String>| {
        let rebased = rebased.unwrap_or_else(|why| Err(Unrebased::InDoubt(why)));
        Event::Rebased { menu, rebased }
    });
    thread::spawn(move || due.pay(Ok(rebased(&on, &dir, &branch))))
}

fn rebased(on: &Machine, dir: &Directory, branch: &str) -> Result<Rebased, Unrebased> {
    match on {
        Machine::Local => rebases::rebase(dir, Some(branch)),
        Machine::Remote(destination) => match Away::reach(destination, away::ANSWER) {
            Ok(mut away) => away.rebase(dir, branch),
            Err(why) => Err(Unrebased::Refused(format!("{destination}: {why}"))),
        },
    }
}

#[must_use]
pub fn said(rebased: &Rebased) -> String {
    let Rebased { branch, onto, came } = rebased;
    match came {
        Came::Already => format!("{branch} is on {onto} already"),
        Came::Clean { ahead: 0 } => format!("{branch} is at {onto} now"),
        Came::Clean { ahead: 1 } => format!("{branch} is rebased onto {onto}, one commit on top"),
        Came::Clean { ahead } => format!("{branch} is rebased onto {onto}, {ahead} commits on top"),
        Came::Stopped(Stopped { conflicted, .. }) => match conflicted.count() {
            0 => format!("the rebase of {branch} onto {onto} stopped"),
            1 => format!("the rebase of {branch} onto {onto} stopped on conflicts in a file"),
            files => {
                format!("the rebase of {branch} onto {onto} stopped on conflicts in {files} files")
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use lunetad::rebases::Conflicted;

    use super::*;

    fn rebased(came: Came) -> Rebased {
        Rebased { branch: "issue-429".to_owned(), onto: "origin/main".to_owned(), came }
    }

    #[test]
    fn what_a_rebase_came_to_is_said_in_a_line() {
        let stopped = |count: usize| {
            let conflicted = Conflicted::of((0..count).map(|it| it.to_string()).collect());
            Came::Stopped(Stopped { at: None, conflicted })
        };
        let said: Vec<String> = [
            Came::Already,
            Came::Clean { ahead: 0 },
            Came::Clean { ahead: 1 },
            Came::Clean { ahead: 3 },
            stopped(0),
            stopped(1),
            stopped(2),
        ]
        .into_iter()
        .map(|came| said(&rebased(came)))
        .collect();
        assert_eq!(said, [
            "issue-429 is on origin/main already",
            "issue-429 is at origin/main now",
            "issue-429 is rebased onto origin/main, one commit on top",
            "issue-429 is rebased onto origin/main, 3 commits on top",
            "the rebase of issue-429 onto origin/main stopped",
            "the rebase of issue-429 onto origin/main stopped on conflicts in a file",
            "the rebase of issue-429 onto origin/main stopped on conflicts in 2 files",
        ]);
    }
}
