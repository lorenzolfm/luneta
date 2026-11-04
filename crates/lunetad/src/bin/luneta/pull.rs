//! A pull request's link, or an issue's, or a `.md` file's path, pasted in
//! luneta, and where the key of the menu it opens goes for it, as
//! `lunetad::links` finds it: on the machine of the row it was pasted on,
//! among that machine's panes, as the sidebar knows them. luneta finds it for
//! a row of its own machine, and
//! the daemon of another for one of that machine's, through the daemon
//! luneta watches. `luneta worktree` is the program that makes the
//! worktree, of wt's code, wherever luneta has it made.

use std::process::ExitCode;
use std::sync::mpsc::Sender;
use std::thread;

use lunetad::links::{self, Pasted};
pub use lunetad::links::{At, Kind, Link, Of, Readied, Work};

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::sidebar::Listed;

/// A pane as the sidebar knows it, for luneta to find where a key of the
/// menu goes: its session and its id, and where it `works`, as
/// `links::Worked` says. The sidebar hands them in the order of its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub session: Listed,
    pub pane: u32,
    pub works: links::Worked,
}

/// What luneta found for a key of the menu, as `links::Ready` says, with
/// each pane by its session and its id, as the sidebar knows it.
pub type Ready = links::Ready<(Listed, u32)>;

/// Finds where a key of the menu numbered `menu` goes for `work`, pasted on a
/// row of the machine `on`, among the panes the sidebar knows of it, `worked`,
/// and has the worktree made there, on a thread of its own; and hands back
/// what it found, or why it found nothing, as `Due` does. luneta finds it on
/// its own machine, and the daemon of another machine there.
pub fn prepare(menu: u64, on: Machine, work: Work, worked: Vec<Worked>, events: &Sender<Event>) {
    let due = Due::new(events, move |ready| Event::Pulled { menu, ready });
    let apart = |Worked { session, pane, works }| ((session, pane), works);
    let (panes, worked): (Vec<(Listed, u32)>, Vec<links::Worked>) =
        worked.into_iter().map(apart).unzip();
    let pasted = Pasted::new(work, worked);
    thread::spawn(move || {
        let ready = match &on {
            Machine::Local => pasted.ready(),
            Machine::Remote(destination) => {
                Away::reach(destination, away::ANSWER).and_then(|mut away| away.paste(&pasted))
            },
        };
        let unasked = |_| match &on {
            Machine::Local => UNASKED.to_owned(),
            Machine::Remote(destination) => format!("{destination}: {UNASKED}"),
        };
        due.pay(ready.and_then(|ready| ready.placed(&panes).map_err(unasked)));
    });
}

/// Why luneta does not go where lunetad found a key goes: to a pane it was
/// not asked of.
const UNASKED: &str = "lunetad says the key goes to a pane luneta did not ask of";

/// Makes a worktree `of` a branch or a pull request, in the repository `repo`
/// is a directory of, as `links::made` does, and prints where it is, or where
/// one of the branch is already, on stdout, where nothing else is: wt prints
/// nothing there, and has git print on stderr. Or prints why it made none,
/// after `links::WHY`, on stderr, after what wt and git say there as they go.
pub fn make(repo: &str, of: &Of) -> ExitCode {
    match links::made(repo, of) {
        Ok(worktree) => {
            println!("{}", worktree.display());
            ExitCode::SUCCESS
        },
        Err(why) => {
            eprintln!("{}{why}", links::WHY);
            ExitCode::FAILURE
        },
    }
}
