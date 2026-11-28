use std::collections::BTreeSet;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::Duration;

use lune_domain::projects::{Dir, Projects, Spot, TerminalViewId, VIEWS_MAX};

use crate::projects::spot;

pub const LOOKS_MAX: usize = 4;

const _: () = assert!(LOOKS_MAX > 0);
const _: () = assert!(LOOKS_MAX <= VIEWS_MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookTimeout(Duration);

impl LookTimeout {
    #[must_use]
    pub const fn of(timeout: Duration) -> LookTimeout {
        assert!(!timeout.is_zero(), "a look has time to run");
        LookTimeout(timeout)
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

struct Look {
    terminal: TerminalViewId,
    folder: Dir,
}

pub struct Spotted {
    terminal: TerminalViewId,
    folder: Dir,
    spot: Result<Spot, String>,
}

impl Spotted {
    pub fn into_parts(self) -> (TerminalViewId, Dir, Result<Spot, String>) {
        (self.terminal, self.folder, self.spot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    Deliver,
    Relook,
    Abandon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InFlight {
    terminal: TerminalViewId,
    after: After,
}

pub struct Homes {
    sent: SyncSender<Look>,
    spotted: Receiver<Spotted>,
    queued: BTreeSet<TerminalViewId>,
    slots: [Option<InFlight>; LOOKS_MAX],
}

impl Homes {
    pub fn start(timeout: LookTimeout) -> io::Result<Homes> {
        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let thread = thread::Builder::new().name("homes".to_owned());
        thread.spawn(move || run(&looks, &found, timeout))?;
        Ok(Homes {
            sent,
            spotted,
            queued: BTreeSet::new(),
            slots: [None; LOOKS_MAX],
        })
    }

    pub fn seed(&mut self, projects: &Projects) {
        for project in projects.all() {
            for (worktree_dir, worktree) in project.placed() {
                for terminal in worktree.layout().terminals() {
                    if *terminal.folder() != worktree_dir {
                        self.mark(terminal.id());
                    }
                }
            }
        }
    }

    pub fn mark(&mut self, terminal: TerminalViewId) {
        if let Some(in_flight) = self.slot_mut(terminal) {
            in_flight.after = After::Relook;
            assert!(
                !self.queued.contains(&terminal),
                "a terminal in flight is not queued"
            );
            return;
        }
        self.queued.insert(terminal);
        assert!(
            self.queued.len() <= VIEWS_MAX,
            "only held terminals wait for a home"
        );
    }

    pub fn forget(&mut self, terminal: TerminalViewId) {
        let was_queued = self.queued.remove(&terminal);
        if let Some(in_flight) = self.slot_mut(terminal) {
            assert!(!was_queued, "a terminal in flight is not queued beside it");
            in_flight.after = After::Abandon;
        }
    }

    pub fn clear(&mut self) {
        self.queued.clear();
        for in_flight in self.slots.iter_mut().flatten() {
            in_flight.after = After::Abandon;
        }
    }

    pub fn dispatch(&mut self, projects: &Projects) {
        assert!(
            self.queued.len() <= VIEWS_MAX,
            "only held terminals wait for a home"
        );
        for _ in 0..LOOKS_MAX {
            let Some(at) = self.slots.iter().position(Option::is_none) else {
                break;
            };
            let Some(terminal) = self.queued.pop_first() else {
                break;
            };
            assert!(
                self.slot_mut(terminal).is_none(),
                "terminal {terminal} is looked for once at a time"
            );
            let held = projects.holding_terminal(terminal.view());
            let (_, held) = held.expect("a terminal waiting for a home is held");
            let sent = self.sent.try_send(Look {
                terminal,
                folder: held.folder().clone(),
            });
            assert!(
                !matches!(sent, Err(TrySendError::Disconnected(_))),
                "the homes run as long as layouts ask"
            );
            assert!(sent.is_ok(), "the homes take every look there is room for");
            self.slots[at] = Some(InFlight {
                terminal,
                after: After::Deliver,
            });
        }
    }

    #[must_use]
    pub fn spotted(&mut self) -> Option<Spotted> {
        for _ in 0..LOOKS_MAX {
            let spotted = match self.spotted.try_recv() {
                Ok(spotted) => spotted,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    unreachable!("the homes run as long as layouts listen")
                }
            };
            let in_flight = self
                .slots
                .iter_mut()
                .find(|slot| slot.is_some_and(|it| it.terminal == spotted.terminal))
                .and_then(Option::take);
            let Some(in_flight) = in_flight else {
                unreachable!(
                    "terminal {} is spotted only once it is looked for",
                    spotted.terminal
                );
            };
            match in_flight.after {
                After::Deliver => {
                    assert!(
                        !self.queued.contains(&spotted.terminal),
                        "a delivered look leaves no duplicate queued"
                    );
                    return Some(spotted);
                }
                After::Relook => {
                    let inserted = self.queued.insert(spotted.terminal);
                    assert!(inserted, "a terminal in flight is not queued beside it");
                    assert!(
                        self.queued.len() <= VIEWS_MAX,
                        "only held terminals wait for a home"
                    );
                }
                After::Abandon => {}
            }
        }
        assert!(self.is_idle(), "draining every slot empties the homes");
        None
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    fn slot_mut(&mut self, terminal: TerminalViewId) -> Option<&mut InFlight> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|it| it.terminal == terminal)
    }
}

fn run(looks: &Receiver<Look>, found: &SyncSender<Spotted>, timeout: LookTimeout) {
    while let Ok(Look { terminal, folder }) = looks.recv() {
        let spot = spot::of(&folder, timeout.get());
        let sent = found.try_send(Spotted {
            terminal,
            folder,
            spot,
        });
        assert!(
            !matches!(sent, Err(TrySendError::Full(_))),
            "spotted holds every look in flight"
        );
        if sent.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::thread;
    use std::time::Duration;

    use lune_domain::projects::{
        Arranged, Change, Dir, Id, Made, Placed, Projects, Ran, Runs, Size, Spot,
    };
    use lune_domain::projects::{Terminal, TerminalViewId, View, WorktreeId};
    use lune_waits::eventually;

    use super::{Homes, LOOKS_MAX, Look, LookTimeout, Spotted, run};
    use crate::panics::contained;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn counting() -> impl FnMut() -> std::io::Result<Id> {
        let mut next = 0;
        move || {
            next += 1;
            Ok(Id::of(next).unwrap())
        }
    }

    fn projects_with_terminals(folders: &[&str]) -> (Projects, WorktreeId, Vec<TerminalViewId>) {
        let mut drawn = counting();
        let mut draw = |projects: &Projects| -> std::io::Result<Id> {
            loop {
                let id = drawn()?;
                if !projects.holds(id) {
                    return Ok(id);
                }
            }
        };
        let mut projects = Projects::default();
        let add = Change::AddProject {
            spot: Spot::in_folder(dir("/work"), None),
            name: None,
        };
        let (next, made) = projects.changed(add, None, || draw(&projects)).unwrap();
        projects = next;
        let Some(Made::Project(project)) = made else {
            panic!("project is made")
        };
        let worktree = projects
            .project_of(project)
            .unwrap()
            .worktrees()
            .next()
            .unwrap()
            .id();
        let size = Size::of(1000, 1000).unwrap();
        let mut ids = Vec::with_capacity(folders.len());
        for &folder in folders {
            let id = projects.fresh_terminal(&mut || draw(&projects)).unwrap();
            let placed = if ids.is_empty() {
                Placed::Alone(Arranged::Tabs)
            } else {
                projects
                    .find(worktree)
                    .unwrap()
                    .1
                    .layout()
                    .room_for(id.view(), None, size)
                    .unwrap()
            };
            let terminal = Terminal::new(id, dir(folder), Runs::Shell);
            let open = Change::Open {
                worktree,
                view: View::Terminal(terminal),
                placed,
                size,
            };
            (projects, _) = projects.changed(open, None, || draw(&projects)).unwrap();
            ids.push(id);
        }
        (projects, worktree, ids)
    }

    fn manual_homes() -> (Homes, Receiver<Look>, SyncSender<Spotted>) {
        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let homes = Homes {
            sent,
            spotted,
            queued: BTreeSet::new(),
            slots: [None; LOOKS_MAX],
        };
        (homes, looks, found)
    }

    #[test]
    fn homes_bounds_in_flight_reuses_slots_and_drops_superseded_or_forgotten_looks() {
        assert!(contained(|| LookTimeout::of(Duration::ZERO)).is_err());

        let folders = ["/work/1", "/work/2", "/work/3", "/work/4", "/work/5"];
        let (mut projects, _, ids) = projects_with_terminals(&folders);
        let (mut homes, looks, found) = manual_homes();
        assert!(homes.is_idle());
        assert!(homes.spotted().is_none());

        for &id in &ids {
            homes.mark(id);
        }
        homes.dispatch(&projects);
        assert!(!homes.is_idle());

        let received: Vec<Look> = (0..LOOKS_MAX).map(|_| looks.recv().unwrap()).collect();
        let looked: Vec<TerminalViewId> = received.iter().map(|it| it.terminal).collect();
        assert_eq!(
            looked,
            ids[..LOOKS_MAX],
            "at most LOOKS_MAX looks are dispatched at once"
        );
        assert!(
            looks.try_recv().is_err(),
            "excess queued terminals wait for a free slot"
        );

        homes.mark(ids[0]);
        homes.forget(ids[1]);

        let dir_of = |i: usize| dir(folders[i]);
        let spot_of = |i: usize| Spot::in_folder(dir_of(i), None);
        found
            .send(Spotted {
                terminal: ids[0],
                folder: dir_of(0),
                spot: Err("stale".to_owned()),
            })
            .unwrap();
        found
            .send(Spotted {
                terminal: ids[1],
                folder: dir_of(1),
                spot: Ok(spot_of(1)),
            })
            .unwrap();
        found
            .send(Spotted {
                terminal: ids[2],
                folder: dir_of(2),
                spot: Ok(spot_of(2)),
            })
            .unwrap();

        let delivered = homes.spotted().unwrap();
        assert_eq!(delivered.into_parts(), (ids[2], dir_of(2), Ok(spot_of(2))));
        assert!(homes.spotted().is_none());

        homes.dispatch(&projects);
        let next: Vec<TerminalViewId> = (0..2).map(|_| looks.recv().unwrap().terminal).collect();
        assert_eq!(next, [ids[0], ids[4]]);
        assert!(looks.try_recv().is_err());

        let ran = Ran {
            terminal: ids[0],
            folder: dir("/work/again"),
            runs: Runs::Shell,
        };
        (projects, _) = projects
            .changed(Change::Ran(vec![ran]), None, counting())
            .unwrap();
        found
            .send(Spotted {
                terminal: ids[0],
                folder: dir_of(0),
                spot: Ok(spot_of(0)),
            })
            .unwrap();
        let delivered = homes.spotted().unwrap();
        assert_eq!(delivered.into_parts(), (ids[0], dir_of(0), Ok(spot_of(0))));
        homes.dispatch(&projects);
        assert!(
            looks.try_recv().is_err(),
            "a delivered terminal leaves no duplicate queued"
        );

        found
            .send(Spotted {
                terminal: ids[3],
                folder: dir_of(3),
                spot: Ok(spot_of(3)),
            })
            .unwrap();
        found
            .send(Spotted {
                terminal: ids[4],
                folder: dir_of(4),
                spot: Ok(spot_of(4)),
            })
            .unwrap();
        assert!(homes.spotted().is_some());
        assert!(homes.spotted().is_some());
        assert!(homes.is_idle());

        let unknown = projects.fresh_terminal(&mut counting()).unwrap();
        found
            .send(Spotted {
                terminal: unknown,
                folder: dir("/work/x"),
                spot: Err("x".to_owned()),
            })
            .unwrap();
        assert!(contained(|| homes.spotted()).is_err());
    }

    #[test]
    fn homes_forget_prunes_queued_and_seed_marks_terminals_away_from_worktree_root() {
        let (projects, _, ids) = projects_with_terminals(&["/work", "/work/sub", "/other/dir"]);
        let (mut homes, looks, found) = manual_homes();

        homes.seed(&projects);
        homes.dispatch(&projects);
        let first = looks.recv().unwrap();
        let second = looks.recv().unwrap();
        assert_eq!(
            [
                (first.terminal, first.folder),
                (second.terminal, second.folder)
            ],
            [(ids[1], dir("/work/sub")), (ids[2], dir("/other/dir"))]
        );
        assert!(
            looks.try_recv().is_err(),
            "terminals at their worktree root are not seeded"
        );

        homes.forget(ids[1]);
        homes.mark(ids[1]);
        homes.forget(ids[2]);
        let sub = dir("/work/sub");
        let other = dir("/other/dir");
        found
            .send(Spotted {
                terminal: ids[1],
                folder: sub.clone(),
                spot: Err("stale".to_owned()),
            })
            .unwrap();
        found
            .send(Spotted {
                terminal: ids[2],
                folder: other,
                spot: Err("stale".to_owned()),
            })
            .unwrap();
        assert!(
            homes.spotted().is_none(),
            "abandoned and relooked slots are not delivered"
        );
        assert!(homes.is_idle());

        homes.dispatch(&projects);
        let relooked = looks.recv().unwrap();
        assert_eq!((relooked.terminal, relooked.folder), (ids[1], sub));
        assert!(
            looks.try_recv().is_err(),
            "abandoned terminal is not re-dispatched"
        );

        homes.mark(ids[0]);
        homes.forget(ids[0]);
        homes.dispatch(&projects);
        assert!(looks.try_recv().is_err());
    }

    #[test]
    fn homes_spots_folders_and_its_thread_ends_when_dropped() {
        let scratch = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(scratch.path()).unwrap();
        let here_path = real.join("here");
        fs::create_dir(&here_path).unwrap();
        let here = dir(here_path.to_str().unwrap());
        let gone = dir(real.join("gone").to_str().unwrap());

        let (projects, _, ids) = projects_with_terminals(&[here.as_str(), gone.as_str()]);
        let (first, second) = (ids[0], ids[1]);
        let limit = LookTimeout::of(Duration::from_secs(5));
        let mut homes = Homes::start(limit).unwrap();
        homes.mark(first);
        homes.mark(second);
        homes.dispatch(&projects);

        let one = eventually("first folder spotted", || homes.spotted()).into_parts();
        let two = eventually("second folder spotted", || homes.spotted()).into_parts();
        assert!(homes.is_idle());
        assert_eq!(
            one,
            (first, here.clone(), Ok(Spot::in_folder(here.clone(), None)))
        );
        assert_eq!(two.0, second);
        assert_eq!(two.1, gone);
        assert!(two.2.unwrap_err().contains("is gone"));
        drop(homes);

        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let worker = thread::spawn(move || run(&looks, &found, limit));
        drop(sent);
        assert!(worker.join().is_ok(), "dropping sent ends the homes thread");
        drop(spotted);

        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        drop(spotted);
        let worker = thread::spawn(move || run(&looks, &found, limit));
        sent.send(Look {
            terminal: first,
            folder: here,
        })
        .unwrap();
        assert!(
            worker.join().is_ok(),
            "dropping spotted ends the homes thread"
        );
    }
}
