//! A pull request's link, or an issue's, or a `.md` file's path, pasted in
//! luneta, and where the key of the menu it opens goes for it, as
//! `lunetad::links` finds it: on the machine of the row it was pasted on,
//! among that machine's panes, as the sidebar knows them. Where that machine
//! has no session in a link's repository and no clone of it, on the first of
//! the other machines that has either: the link says what it is of, and only
//! a machine that has it can act on it. A file's path is of the row's machine
//! alone. luneta finds it for a row of its own machine, and
//! the daemon of another for one of that machine's, through the daemon
//! luneta watches. `luneta worktree` is the program that makes the
//! worktree, of wt's code, wherever luneta has it made.

use std::process::ExitCode;
use std::sync::mpsc::Sender;
use std::thread;

use lunetad::links::{self, Pasted, Unready};
pub use lunetad::links::{At, Kind, Link, Of, Readied, Wanted, Work};
use lunetad::log;

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

/// What luneta found for a key of the menu, and the machine it found it on,
/// which that key acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub on: Machine,
    pub ready: Ready,
}

/// The machines other than the row's, in the order of the rows, each with the
/// panes the sidebar knows of it, or why it knows none.
pub type Others = Vec<(Machine, Result<Vec<Worked>, String>)>;

/// What this machine goes by in a note, beside the others' destinations.
const THIS: &str = "this machine";

/// Finds where a key of the menu numbered `menu` goes for `work`, pasted on a
/// row of the machine `on`, among the panes the sidebar knows of it, `worked`,
/// or else of the machines `others`, as `first_found` says, and has the
/// worktree made there, as `wanted`, on a thread of its own; and hands back
/// what it found, or why it found nothing, as `Due` does. luneta finds it on
/// its own machine, and the daemon of another machine there.
pub fn prepare(
    menu: u64,
    on: Machine,
    work: Work,
    wanted: Wanted,
    worked: Vec<Worked>,
    others: Others,
    events: &Sender<Event>,
) {
    let due = Due::new(events, move |found| Event::Pulled { menu, found });
    thread::spawn(move || due.pay(first_found(on, &work, wanted, worked, others, asked)));
}

/// Where a key of the menu goes on the machine `on`, by what that machine
/// says of `pasted`.
fn asked(on: &Machine, pasted: &Pasted) -> Result<links::Ready<usize>, Unready> {
    match on {
        Machine::Local => pasted.ready(),
        Machine::Remote(destination) => {
            let reached = Away::reach(destination, away::ANSWER);
            let unreached = |why| Unready::Failed(format!("{destination}: {why}"));
            reached.map_err(unreached)?.paste(pasted)
        },
    }
}

/// Where a key of the menu goes for `work`, by what `ask` has each machine
/// say: the machine `on`, among `worked`; and, of a link, where that has
/// neither a session in its repository nor a clone of it, each of `others`
/// in turn, until one has either. Anything else that goes wrong on `on` ends
/// it: that machine has the repository, or may. One of `others` that made
/// nothing is passed over, and said only if none has it; it is in the log
/// otherwise. One that may have made the worktree ends it too. Each is asked
/// after the last answers, as the first that has it makes a worktree there,
/// and no other should.
fn first_found(
    on: Machine,
    work: &Work,
    wanted: Wanted,
    worked: Vec<Worked>,
    others: Others,
    mut ask: impl FnMut(&Machine, &Pasted) -> Result<links::Ready<usize>, Unready>,
) -> Result<Found, String> {
    let mut found_on = |on: Machine, worked: Vec<Worked>| {
        let apart = |Worked { session, pane, works }| ((session, pane), works);
        let (panes, worked): (Vec<(Listed, u32)>, Vec<links::Worked>) =
            worked.into_iter().map(apart).unzip();
        let ready = ask(&on, &Pasted::new(work.clone(), worked).wanting(wanted))?;
        match ready.placed(&panes) {
            Ok(ready) => Ok(Found { on, ready }),
            // It made the worktree by now.
            Err(_) => Err(Unready::InDoubt(match &on {
                Machine::Local => UNASKED.to_owned(),
                Machine::Remote(destination) => format!("{destination}: {UNASKED}"),
            })),
        }
    };
    let why = match found_on(on.clone(), worked) {
        Ok(found) => return Ok(found),
        Err(Unready::Failed(why) | Unready::InDoubt(why)) => return Err(why),
        Err(Unready::Nowhere(why)) => why,
    };
    let (Work::Link(link), false) = (work, others.is_empty()) else {
        return Err(why);
    };
    let mut nowhere = vec![on];
    let mut failed: Vec<String> = Vec::new();
    for (other, worked) in others {
        match worked.map_err(Unready::Failed).and_then(|worked| found_on(other.clone(), worked)) {
            Ok(found) => {
                for why in &failed {
                    log::say(why);
                }
                return Ok(found);
            },
            Err(Unready::Nowhere(_)) => nowhere.push(other),
            Err(Unready::Failed(why)) => failed.push(naming(&other, why)),
            Err(Unready::InDoubt(why)) => {
                let why = naming(&other, why);
                let note = format!(
                    "{why}; it may have made the worktree there, so luneta asked no other machine"
                );
                return Err(std::iter::once(note).chain(failed).collect::<Vec<_>>().join("; "));
            },
        }
    }
    let on = nowhere.iter().map(|machine| match machine {
        Machine::Local => THIS.to_owned(),
        Machine::Remote(destination) => destination.to_string(),
    });
    let note = format!(
        "no session on {} works in {}, nor does zoxide there know a clone of it; wt clone it first",
        on.collect::<Vec<_>>().join(" or "),
        link.repository(),
    );
    Err(std::iter::once(note).chain(failed).collect::<Vec<_>>().join("; "))
}

/// `why`, as a note says it among those of other machines: of another machine
/// than this, it says which already.
fn naming(machine: &Machine, why: String) -> String {
    match machine {
        Machine::Local => format!("{THIS}: {why}"),
        Machine::Remote(_) => why,
    }
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

/// Brings the worktree in `dir` to its upstream branch, as `links::followed`
/// does. Or prints why it did not, as `make` does.
pub fn follow(dir: &str) -> ExitCode {
    match links::followed(dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{}{why}", links::WHY);
            ExitCode::FAILURE
        },
    }
}

/// Fetches the upstream branch of `branch`, of the repository `dir` is in, as
/// `links::fetched_upstream` does, and prints the word for what wt found. Or
/// prints why it did not, as `make` does.
pub fn fetch_upstream(dir: &str, branch: &str) -> ExitCode {
    match links::fetched_upstream(dir, branch) {
        Ok(found) => {
            println!("{}", links::upstream_word(found));
            ExitCode::SUCCESS
        },
        Err(why) => {
            eprintln!("{}{why}", links::WHY);
            ExitCode::FAILURE
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, SystemTime};

    use lunetad::Destination;

    use super::*;

    fn far() -> Destination {
        "far".parse().unwrap()
    }

    fn vps() -> Destination {
        "me@vps".parse().unwrap()
    }

    fn link() -> Work {
        Work::Link(Link::parse("https://github.com/cashubtc/cdk/pull/5").unwrap())
    }

    /// A pane of its machine's session `main` that works in `dir`.
    fn pane(id: u32, dir: &str) -> Worked {
        let works = links::Worked { dir: dir.to_owned(), pull: None, here: false };
        let session = Listed::new("main", SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        Worked { session, pane: id, works }
    }

    /// What a machine says where its pane of the place `at` is where a key goes.
    fn there(at: usize) -> links::Ready<usize> {
        let work = Readied::Link(Link::parse("https://github.com/cashubtc/cdk/pull/5").unwrap());
        let worktree = "/w/x".to_owned();
        links::Ready { at: At::Session(at), worktree, branch: None, moved_off: false, work }
    }

    fn nowhere() -> Result<links::Ready<usize>, Unready> {
        Err(Unready::Nowhere("no session works in cashubtc/cdk".to_owned()))
    }

    /// What `first_found` finds of `work`, pasted on a row of this machine,
    /// with the machines of `others`, each answering as `say` has it; and the
    /// machines it asked, in the order it asked them.
    fn asking(
        work: &Work,
        others: Others,
        say: impl Fn(&Machine) -> Result<links::Ready<usize>, Unready>,
    ) -> (Result<Found, String>, Vec<Machine>) {
        let asked = RefCell::new(Vec::new());
        let panes = vec![pane(1, "/m")];
        let found = first_found(Machine::Local, work, Wanted::AsItIs, panes, others, |on, _| {
            asked.borrow_mut().push(on.clone());
            say(on)
        });
        (found, asked.into_inner())
    }

    fn remote(
        destination: Destination,
        panes: Vec<Worked>,
    ) -> (Machine, Result<Vec<Worked>, String>) {
        (Machine::Remote(destination), Ok(panes))
    }

    /// A machine that has the repository is where the key goes, by its own
    /// panes, and no other is asked, so that no other makes a worktree. The
    /// row's goes first.
    #[test]
    fn the_row_s_machine_goes_first_and_the_first_that_has_it_ends_the_search() {
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&link(), others, |_| Ok(there(0)));
        let Found { on, ready } = found.unwrap();
        assert_eq!((on, asked), (Machine::Local, vec![Machine::Local]));
        assert_eq!(ready.at, At::Session((pane(1, "/m").session, 1)));
    }

    /// A row of a machine that has neither a session in the repository nor a
    /// clone of it goes to the next that has either, in the order given, and
    /// the key acts on that machine, by the pane it said.
    #[test]
    fn a_row_of_a_machine_that_has_none_of_the_repository_goes_to_the_next_that_does() {
        let others = vec![
            remote(far(), vec![pane(7, "/f")]),
            remote(vps(), vec![pane(8, "/v"), pane(9, "/w")]),
        ];
        let (found, asked) = asking(&link(), others, |on| match on {
            Machine::Remote(destination) if *destination == vps() => Ok(there(1)),
            Machine::Local | Machine::Remote(_) => nowhere(),
        });
        let Found { on, ready } = found.unwrap();
        assert_eq!(on, Machine::Remote(vps()));
        assert_eq!(ready.at, At::Session((pane(9, "/w").session, 9)));
        let order = vec![Machine::Local, Machine::Remote(far()), Machine::Remote(vps())];
        assert_eq!(asked, order);
    }

    #[test]
    fn each_machine_hears_what_the_key_wants_of_the_worktree() {
        for (wanted, at_head) in [(Wanted::AsItIs, false), (Wanted::AtHead, true)] {
            let others = vec![remote(far(), vec![pane(7, "/f")])];
            let heard = RefCell::new(Vec::new());
            let found = first_found(Machine::Local, &link(), wanted, Vec::new(), others, |on, it| {
                heard.borrow_mut().push(it.asked(None).at_head);
                if *on == Machine::Local { nowhere() } else { Ok(there(0)) }
            });
            assert!(found.is_ok(), "{found:?}");
            assert_eq!(heard.into_inner(), [at_head, at_head]);
        }
    }

    /// What goes wrong on the row's machine ends the search: it says what
    /// stands in the way there, as a clone git made, and going elsewhere
    /// would hide it.
    #[test]
    fn what_goes_wrong_on_the_row_s_machine_goes_no_further() {
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&link(), others, |_| {
            Err(Unready::Failed("/w/cdk is in a clone git made".to_owned()))
        });
        assert_eq!(found, Err("/w/cdk is in a clone git made".to_owned()));
        assert_eq!(asked, [Machine::Local]);
    }

    /// With no other machine, the note says what the row's said, by itself.
    /// With others, it names each that has none of it, and says why of each
    /// that did not answer, a machine lunetad does not reach among them,
    /// which is not asked.
    #[test]
    fn the_note_names_the_machines_it_tried() {
        let alone = asking(&link(), Vec::new(), |_| nowhere()).0;
        assert_eq!(alone, Err("no session works in cashubtc/cdk".to_owned()));

        let others = vec![remote(far(), vec![pane(7, "/f")]), remote(vps(), Vec::new())];
        let all = asking(&link(), others, |_| nowhere()).0;
        let none = "no session on this machine or far or me@vps works in cashubtc/cdk, nor does \
                    zoxide there know a clone of it; wt clone it first";
        assert_eq!(all, Err(none.to_owned()));

        let others = vec![
            (Machine::Remote(vps()), Err("lunetad does not reach me@vps now".to_owned())),
            remote(far(), vec![pane(7, "/f")]),
        ];
        let (said, asked) = asking(&link(), others, |on| match on {
            Machine::Local => nowhere(),
            Machine::Remote(_) => Err(Unready::Failed("far: wt add: it failed".to_owned())),
        });
        let tried = "no session on this machine works in cashubtc/cdk, nor does zoxide there know \
                     a clone of it; wt clone it first; lunetad does not reach me@vps now; far: wt \
                     add: it failed";
        assert_eq!(said, Err(tried.to_owned()));
        assert_eq!(asked, [Machine::Local, Machine::Remote(far())]);
    }

    /// One of the others that fails is passed over for one that has it. This
    /// machine, among the others of a row of another, is named in the note.
    #[test]
    fn another_machine_that_fails_is_passed_over() {
        let others = vec![
            (Machine::Remote(vps()), Err("lunetad does not reach me@vps now".to_owned())),
            remote(far(), vec![pane(7, "/f")]),
        ];
        let (found, _) = asking(&link(), others, |on| match on {
            Machine::Local => nowhere(),
            Machine::Remote(_) => Ok(there(0)),
        });
        assert_eq!(found.unwrap().on, Machine::Remote(far()));

        let others = vec![(Machine::Local, Ok(Vec::new()))];
        let far = Machine::Remote(far());
        let said = first_found(far, &link(), Wanted::AsItIs, Vec::new(), others, |on, _| match on {
            Machine::Local => Err(Unready::Failed("zoxide is no program".to_owned())),
            Machine::Remote(_) => nowhere(),
        });
        let note = "no session on far works in cashubtc/cdk, nor does zoxide there know a clone of \
                    it; wt clone it first; this machine: zoxide is no program";
        assert_eq!(said, Err(note.to_owned()));
    }

    /// One of the others that may have made the worktree ends the search, so
    /// that the next makes no second one: as it says so, or as it says a pane
    /// luneta did not ask of, having made it. What others said is in the note.
    #[test]
    fn a_machine_that_may_have_made_the_worktree_ends_the_search() {
        let others = || {
            vec![
                (Machine::Remote(vps()), Err("lunetad does not reach me@vps now".to_owned())),
                remote(far(), vec![pane(7, "/f")]),
                remote("pi".parse().unwrap(), vec![pane(8, "/p")]),
            ]
        };
        let (said, asked) = asking(&link(), others(), |on| match on {
            Machine::Local => nowhere(),
            Machine::Remote(_) => Err(Unready::InDoubt("far: no answer".to_owned())),
        });
        let note = "far: no answer; it may have made the worktree there, so luneta asked no other \
                    machine; lunetad does not reach me@vps now";
        assert_eq!(said, Err(note.to_owned()));
        assert_eq!(asked, [Machine::Local, Machine::Remote(far())]);

        let (said, asked) = asking(&link(), others(), |on| match on {
            Machine::Local => nowhere(),
            Machine::Remote(_) => Ok(there(3)),
        });
        assert!(said.unwrap_err().starts_with(&format!("far: {UNASKED}; it may have")));
        assert_eq!(asked, [Machine::Local, Machine::Remote(far())]);

        // This machine, among the others of a row of another, is named.
        let others = vec![(Machine::Local, Ok(vec![pane(1, "/m")])), remote(vps(), Vec::new())];
        let on = Machine::Remote(far());
        let said = first_found(on, &link(), Wanted::AsItIs, Vec::new(), others, |on, _| match on {
            Machine::Local => Err(Unready::InDoubt("no answer".to_owned())),
            Machine::Remote(_) => nowhere(),
        });
        assert!(said.unwrap_err().starts_with("this machine: no answer; it may have"));
    }

    /// A file's path is of the row's machine alone: one of its name elsewhere
    /// is no file of this.
    #[test]
    fn a_file_goes_to_no_other_machine() {
        let file = Work::File(links::File::parse("/w/plan.md", None).unwrap());
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&file, others, |_| nowhere());
        assert_eq!(found, Err("no session works in cashubtc/cdk".to_owned()));
        assert_eq!(asked, [Machine::Local]);
    }

    /// A pane no one asked of is no place to go, and says so, by its machine.
    #[test]
    fn a_pane_luneta_did_not_ask_of_is_no_place_to_go() {
        let on = Machine::Remote(far());
        let found =
            first_found(on, &link(), Wanted::AsItIs, Vec::new(), Vec::new(), |_, _| Ok(there(3)));
        assert_eq!(found, Err(format!("far: {UNASKED}")));
    }
}
