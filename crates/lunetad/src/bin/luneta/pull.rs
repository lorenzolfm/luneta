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
use crate::sidebar::{Listed, PaneKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub session: Listed,
    pub pane: PaneKey,
    pub works: links::Worked,
}

pub type Ready = links::Ready<(Listed, PaneKey)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub on: Machine,
    pub ready: Ready,
}

pub type Others = Vec<(Machine, Result<Vec<Worked>, String>)>;

const THIS: &str = "this machine";

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
        let (panes, worked): (Vec<(Listed, PaneKey)>, Vec<links::Worked>) =
            worked.into_iter().map(apart).unzip();
        let ready = ask(&on, &Pasted::new(work.clone(), worked).wanting(wanted))?;
        match ready.placed(&panes) {
            Ok(ready) => Ok(Found { on, ready }),
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

fn naming(machine: &Machine, why: String) -> String {
    match machine {
        Machine::Local => format!("{THIS}: {why}"),
        Machine::Remote(_) => why,
    }
}

const UNASKED: &str = "lunetad says the key goes to a pane luneta did not ask of";

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

pub fn follow(dir: &str) -> ExitCode {
    match links::followed(dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{}{why}", links::WHY);
            ExitCode::FAILURE
        },
    }
}

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

    fn pane(id: u32, dir: &str) -> Worked {
        let works = links::Worked { dir: dir.to_owned(), pull: None, here: false };
        let session = Listed::new("main", SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        Worked { session, pane: PaneKey::Zellij(id), works }
    }

    fn there(at: usize) -> links::Ready<usize> {
        let work = Readied::Link(Link::parse("https://github.com/cashubtc/cdk/pull/5").unwrap());
        let worktree = "/w/x".to_owned();
        links::Ready { at: At::Session(at), worktree, branch: None, moved_off: false, work }
    }

    fn nowhere() -> Result<links::Ready<usize>, Unready> {
        Err(Unready::Nowhere("no session works in cashubtc/cdk".to_owned()))
    }

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

    #[test]
    fn the_row_s_machine_goes_first_and_the_first_that_has_it_ends_the_search() {
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&link(), others, |_| Ok(there(0)));
        let Found { on, ready } = found.unwrap();
        assert_eq!((on, asked), (Machine::Local, vec![Machine::Local]));
        assert_eq!(ready.at, At::Session((pane(1, "/m").session, PaneKey::Zellij(1))));
    }

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
        assert_eq!(ready.at, At::Session((pane(9, "/w").session, PaneKey::Zellij(9))));
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

    #[test]
    fn what_goes_wrong_on_the_row_s_machine_goes_no_further() {
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&link(), others, |_| {
            Err(Unready::Failed("/w/cdk is in a clone git made".to_owned()))
        });
        assert_eq!(found, Err("/w/cdk is in a clone git made".to_owned()));
        assert_eq!(asked, [Machine::Local]);
    }

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

        let others = vec![(Machine::Local, Ok(vec![pane(1, "/m")])), remote(vps(), Vec::new())];
        let on = Machine::Remote(far());
        let said = first_found(on, &link(), Wanted::AsItIs, Vec::new(), others, |on, _| match on {
            Machine::Local => Err(Unready::InDoubt("no answer".to_owned())),
            Machine::Remote(_) => nowhere(),
        });
        assert!(said.unwrap_err().starts_with("this machine: no answer; it may have"));
    }

    #[test]
    fn a_file_goes_to_no_other_machine() {
        let file = Work::File(links::File::parse("/w/plan.md", None).unwrap());
        let others = vec![remote(far(), vec![pane(7, "/f")])];
        let (found, asked) = asking(&file, others, |_| nowhere());
        assert_eq!(found, Err("no session works in cashubtc/cdk".to_owned()));
        assert_eq!(asked, [Machine::Local]);
    }

    #[test]
    fn a_pane_luneta_did_not_ask_of_is_no_place_to_go() {
        let on = Machine::Remote(far());
        let found =
            first_found(on, &link(), Wanted::AsItIs, Vec::new(), Vec::new(), |_, _| Ok(there(3)));
        assert_eq!(found, Err(format!("far: {UNASKED}")));
    }
}
