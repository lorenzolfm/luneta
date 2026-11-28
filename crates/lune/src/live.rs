use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use lune_domain::projects::ViewId;

use crate::ledger::{Key, Mark, Session};
use crate::machines::Machine;

pub const GONE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct Lists {
    projects: HashMap<Session, (Machine, HashSet<ViewId>)>,
    unsure: HashSet<Machine>,
}

impl Lists {
    pub fn lists<V: IntoIterator<Item = ViewId>>(
        &mut self,
        machine: &Machine,
        sessions: impl IntoIterator<Item = (Session, V)>,
        whole: bool,
    ) {
        for (session, views) in sessions {
            self.projects
                .insert(session, (machine.clone(), views.into_iter().collect()));
        }
        if !whole {
            self.unsure.insert(machine.clone());
        }
    }

    pub fn unreached(&mut self, machine: &Machine) {
        self.unsure.insert(machine.clone());
    }
}

pub trait Runs: Mark {
    fn heard(self, live: &Lists) -> Option<(&Machine, bool)>;
}

impl Runs for Session {
    fn heard(self, live: &Lists) -> Option<(&Machine, bool)> {
        live.projects.get(&self).map(|(machine, _)| (machine, true))
    }
}

impl Runs for Key {
    fn heard(self, live: &Lists) -> Option<(&Machine, bool)> {
        let (machine, views) = live.projects.get(&self.session)?;
        Some((machine, views.contains(&self.view)))
    }
}

#[derive(Debug)]
pub struct Missing<R> {
    since: BTreeMap<R, Instant>,
    ran_on: BTreeMap<R, Machine>,
}

impl<R> Default for Missing<R> {
    fn default() -> Self {
        Missing {
            since: BTreeMap::new(),
            ran_on: BTreeMap::new(),
        }
    }
}

impl<R: Runs> Missing<R> {
    pub fn gone(&mut self, kept: &BTreeSet<R>, live: Option<&Lists>, at: Instant) -> BTreeSet<R> {
        self.since.retain(|mark, _| kept.contains(mark));
        self.ran_on.retain(|mark, _| kept.contains(mark));
        let Some(live) = live else {
            self.since.clear();
            return BTreeSet::new();
        };
        let mut gone = BTreeSet::new();
        for mark in kept {
            if let Some((machine, listed)) = mark.heard(live) {
                self.ran_on.insert(*mark, machine.clone());
                if listed {
                    self.since.remove(mark);
                    continue;
                }
            }
            let unsure = match self.ran_on.get(mark) {
                Some(machine) => live.unsure.contains(machine),
                None => !live.unsure.is_empty(),
            };
            if unsure {
                self.since.remove(mark);
                continue;
            }
            let since = *self.since.entry(*mark).or_insert(at);
            if at.duration_since(since) >= GONE {
                gone.insert(*mark);
            }
        }
        for mark in &gone {
            self.since.remove(mark);
            self.ran_on.remove(mark);
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(n: u64) -> Session {
        Session::Project(lune_domain::projects::ProjectId::of(n).unwrap())
    }

    fn lists<const N: usize>(machines: [(&Machine, &[Session], bool); N]) -> Lists {
        let mut live = Lists::default();
        for (machine, sessions, whole) in machines {
            live.lists(
                machine,
                sessions.iter().map(|session| (*session, [])),
                whole,
            );
        }
        live
    }

    #[test]
    fn a_session_ends_by_the_list_of_the_machine_it_ran_on() {
        let (here, vps) = (Machine::Local, Machine::Remote("me@vps".parse().unwrap()));
        let [mine, its, unheard] = [1, 2, 3].map(session);
        let kept = BTreeSet::from([mine, its, unheard]);
        let mut missing = Missing::default();
        let start = Instant::now();
        let both = lists([(&here, &[mine], true), (&vps, &[its], true)]);
        assert!(missing.gone(&kept, Some(&both), start).is_empty());

        let unreached = lists([(&here, &[], true), (&vps, &[], false)]);
        assert!(missing.gone(&kept, Some(&unreached), start).is_empty());
        let gone = missing.gone(&kept, Some(&unreached), start + GONE);
        assert_eq!(gone, BTreeSet::from([mine]));

        let kept = BTreeSet::from([its, unheard]);
        let forgotten = lists([(&here, &[], true)]);
        assert!(
            missing
                .gone(&kept, Some(&forgotten), start + GONE * 2)
                .is_empty()
        );
        assert_eq!(
            missing.gone(&kept, Some(&forgotten), start + GONE * 3),
            kept
        );
    }
}
