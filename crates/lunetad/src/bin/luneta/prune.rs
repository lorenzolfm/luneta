//! `luneta prune`, and what the menu's `d m` asks for: the worktrees of a
//! repository whose pull requests are merged, as `lunetad::prunes` finds them
//! on the machine it runs on, and for a far row, on that row's machine.

use std::collections::BTreeSet;
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use lunetad::asks::Turn;
use lunetad::prunes::{
    self, Came, Deleting, Goes, Occupied, Outcome, Plan, Planned, Pruned, Pushed, name_of,
};
use lunetad::Directory;

use crate::away::{self, Away};
use crate::commands;
use crate::drive::{self, Agreed, PLANNED};
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;

const NO_FOLDER: &str = "neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in";

/// Says what deleting the merged worktrees of the repository of `repo`, or of
/// this directory, deletes, and deletes it where `yes` is the plan it says, or
/// in luneta's ask box, the plan it said in the turn before. Nothing goes while
/// lunetad does not say where the panes are.
pub fn run(repo: Option<&Path>, yes: Option<Agreed>, folder: &Path) -> ExitCode {
    let turn = match drive::turn_here() {
        Ok(turn) => turn,
        Err(why) => return failed(&why),
    };
    let dir = match drive::directory(repo).and_then(Directory::parse) {
        Ok(dir) => dir,
        Err(why) => return failed(&why),
    };
    let occupied = match commands::occupied(folder) {
        Ok(occupied) => occupied,
        Err(why) => return failed(&untold(&why)),
    };
    let here = env::current_dir().ok();
    let planned = match prunes::plan(&dir, &occupied, here.as_deref()) {
        Ok(planned) => planned,
        Err(why) => return failed(&why),
    };
    let plan = planned.plan();
    for line in listed(plan) {
        println!("{line}");
    }
    if plan.goes.is_empty() {
        let name = plan.repository_name();
        if plan.unasked.is_empty() {
            eprintln!("luneta: {name} has no merged worktree to delete");
            return ExitCode::SUCCESS;
        }
        return failed(&format!(
            "{name}: no merged worktree to delete of those it could ask about"
        ));
    }
    let agreed = Agreed::of_prune(plan, turn.map(Turn::key));
    let takes = match turn {
        None => Some(agreed),
        Some(turn) => turn.before().map(|before| Agreed::of_prune(plan, Some(before))),
    };
    if yes.is_some() && yes == takes {
        let occupied = || commands::occupied(folder);
        return said_of(&prunes::deleted(planned, occupied, here.as_deref()));
    }
    eprintln!("luneta: {}", unagreed(yes, agreed));
    ExitCode::from(PLANNED)
}

fn unagreed(yes: Option<Agreed>, agreed: Agreed) -> String {
    match yes {
        Some(yes) if yes == agreed => format!(
            "deleted nothing, as luneta's ask box takes a yes to a plan only in the turn after the \
             one that said it, with the user's answer between them: end your turn asking them, and \
             once they say yes, run it again with --yes {agreed}"
        ),
        Some(_) => format!(
            "deleted nothing, as it was told yes to another plan than this; run it again with \
             --yes {agreed} to delete what it says now"
        ),
        None => format!("deleted nothing; run it again with --yes {agreed} to delete what it says"),
    }
}

fn failed(why: &str) -> ExitCode {
    eprintln!("luneta: {why}");
    ExitCode::FAILURE
}

fn untold(why: &str) -> String {
    format!("luneta cannot tell where the panes are, so it deletes nothing: {why}")
}

fn said_of(outcomes: &[Outcome]) -> ExitCode {
    let mut well = true;
    for outcome in outcomes {
        let name = name_of(&outcome.worktree);
        match &outcome.came {
            Came::Gone => println!("deleted {name}"),
            Came::WentBut(why) => {
                println!("deleted {name}");
                eprintln!("luneta: {name}: {why}");
                well = false;
            },
            Came::Stays(why) => {
                eprintln!("luneta: {name} stays: {why}");
                well = false;
            },
        }
    }
    if well { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// What it takes up, in the words `du -h` has for it.
pub fn size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;
    const GIB: u64 = MIB * KIB;
    let in_tenths = |unit: u64| (u128::from(bytes) * 10 + u128::from(unit) / 2) / u128::from(unit);
    match bytes {
        GIB.. => {
            let tenths = in_tenths(GIB);
            format!("{}.{} GB", tenths / 10, tenths % 10)
        },
        MIB.. => format!("{} MB", bytes.div_ceil(MIB)),
        _ => format!("{} KB", bytes.div_ceil(KIB)),
    }
}

/// `goes`'s line of what the plan deletes: what goes with its worktree.
fn with_it(goes: &Goes) -> String {
    let branch = &goes.branch;
    match &goes.pushed {
        Pushed::Nowhere => format!("branch {branch}"),
        Pushed::Goes(far) => format!("branch {branch}, and {} on {}", far.branch, far.remote),
        Pushed::Stays(why) => format!("branch {branch}; what is pushed of it stays: {why}"),
    }
}

/// What the CLI lists of `plan`, a line to each worktree it says of.
fn listed(plan: &Plan) -> Vec<String> {
    let width = plan.goes.iter().map(|goes| name_of(&goes.worktree).len()).max().unwrap_or(0);
    let goes = plan.goes.iter().map(|goes| {
        let (name, number) = (name_of(&goes.worktree), goes.number);
        let bytes = goes.bytes.map_or("?".to_owned(), size);
        format!("delete {name:<width$}  #{number}  {bytes}  {}", with_it(goes))
    });
    let kept = plan
        .kept
        .iter()
        .map(|kept| format!("keep {} #{}: {}", name_of(&kept.worktree), kept.number, kept.why));
    let unasked = plan
        .unasked
        .iter()
        .map(|unasked| format!("keep {}: {}", name_of(&unasked.worktree), unasked.why));
    goes.chain(kept).chain(unasked).collect()
}

/// Asks what deleting the merged worktrees of the repository `dir` is in, on
/// the machine `on`, deletes, for the menu numbered `menu`, on a thread of its
/// own, and hands back what it says, as `Due` does.
pub fn plan(menu: u64, on: Machine, dir: Directory, events: &Sender<Event>) {
    let due = Due::new(events, move |plan| Event::Prunes { menu, plan });
    thread::spawn(move || due.pay(planned(&on, &dir)));
}

fn planned(on: &Machine, dir: &Directory) -> Result<Plan, String> {
    match on {
        Machine::Local => {
            let here = env::current_dir().ok();
            let occupied = local_occupied().map_err(|why| untold(&why))?;
            prunes::plan(dir, &occupied, here.as_deref()).map(Planned::into_plan)
        },
        Machine::Remote(destination) => Away::reach(destination, away::ANSWER)?.prunes(dir),
    }
}

fn local_occupied() -> Result<Occupied, String> {
    let folder = lunetad::folder::find().ok_or(NO_FOLDER)?;
    commands::occupied(&folder)
}

/// Deletes what would go of the repository `dir` is in, on the machine `on`,
/// where that is `agreed`, for the menu numbered `menu`, on a thread of its own,
/// and hands back what came of that, as `Due` does.
pub fn delete(
    menu: u64,
    on: Machine,
    dir: Directory,
    agreed: BTreeSet<Deleting>,
    events: &Sender<Event>,
) -> JoinHandle<()> {
    let due = Due::new(events, move |pruned| Event::Pruned { menu, pruned });
    thread::spawn(move || due.pay(deleted(&on, &dir, &agreed)))
}

fn deleted(on: &Machine, dir: &Directory, agreed: &BTreeSet<Deleting>) -> Result<Pruned, String> {
    match on {
        Machine::Local => {
            let here = env::current_dir().ok();
            prunes::deleted_as_agreed(dir, agreed, local_occupied, here.as_deref())
        },
        Machine::Remote(destination) => Away::reach(destination, away::ANSWER)?.prune(dir, agreed),
    }
}

/// What a note says of `outcomes`, where anything stays that was to go: which,
/// and why.
pub fn note(outcomes: &[Outcome]) -> Option<String> {
    let mut stays: Vec<String> = Vec::new();
    let mut gone = 0;
    for outcome in outcomes {
        let name = name_of(&outcome.worktree);
        match &outcome.came {
            Came::Gone => gone += 1,
            Came::WentBut(why) => {
                gone += 1;
                stays.push(format!("{name} went, but {why}"));
            },
            Came::Stays(why) => stays.push(format!("{name} stays: {why}")),
        }
    }
    if stays.is_empty() {
        return None;
    }
    Some(format!("menu: deleted {gone} of {} worktrees; {}", outcomes.len(), stays.join("; ")))
}

#[cfg(test)]
mod tests {
    use lunetad::prunes::RemoteBranch;

    use super::*;

    #[test]
    fn a_size_reads_as_du_writes_it() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(size(0), "0 KB");
        assert_eq!(size(1), "1 KB");
        assert_eq!(size(4 * 1024), "4 KB");
        assert_eq!(size(916 * MIB), "916 MB");
        assert_eq!(size(4_679_972 * 1024), "4.5 GB");
        assert_eq!(size(1024 * MIB - 1), "1024 MB");
        assert_eq!(size(1024 * MIB), "1.0 GB");
    }

    fn outcome(name: &str, came: Came) -> Outcome {
        Outcome { worktree: format!("/w/{name}"), came }
    }

    /// A note says which stayed and why, and none where everything went.
    #[test]
    fn a_note_names_what_stays() {
        assert_eq!(note(&[outcome("a", Came::Gone)]), None);
        let said = note(&[
            outcome("a", Came::Gone),
            outcome("b", Came::Stays("it has a pane".to_owned())),
            outcome("c", Came::WentBut("its branch on origin stays: no key".to_owned())),
        ]);
        let want = "menu: deleted 2 of 3 worktrees; b stays: it has a pane; c went, but its branch \
                    on origin stays: no key";
        assert_eq!(said.as_deref(), Some(want));
    }

    /// The list has every worktree, its number and its size, what goes with
    /// it, and why the rest stay.
    #[test]
    fn the_list_says_what_goes_and_why_the_rest_stay() {
        let goes = |name: &str, pushed, bytes| Goes {
            worktree: format!("/w/{name}"),
            branch: name.to_owned(),
            head: "abc".to_owned(),
            number: 7,
            pushed,
            bytes,
        };
        let plan = Plan {
            repository: "/w".to_owned(),
            goes: vec![
                goes(
                    "one",
                    Pushed::Goes(RemoteBranch {
                        remote: "origin".to_owned(),
                        branch: "one".to_owned(),
                    }),
                    Some(4096),
                ),
                goes("two-long", Pushed::Stays("moved".to_owned()), None),
                goes("tre", Pushed::Nowhere, Some(4096)),
            ],
            kept: vec![prunes::Kept {
                worktree: "/w/k".to_owned(),
                number: 8,
                why: "it has a pane".to_owned(),
            }],
            unasked: vec![prunes::Unasked {
                worktree: "/w/u".to_owned(),
                why: "gh failed".to_owned(),
            }],
        };
        assert_eq!(
            listed(&plan),
            [
                "delete one       #7  4 KB  branch one, and one on origin",
                "delete two-long  #7  ?  branch two-long; what is pushed of it stays: moved",
                "delete tre       #7  4 KB  branch tre",
                "keep k #8: it has a pane",
                "keep u: gh failed",
            ]
        );
    }
}
